//! ALTER TABLE for the Structure view: column and index changes are staged as `Change`s, several of
//! them make one statement, and the statements that undo them are worked out from the structure read
//! before applying. Everything here but the worker part is pure text building. Names go through
//! `quote_ident`, strings through `lit`, and the few places that take SQL from the user (an expression
//! default) are checked. Every statement runs as a prepared statement, which holds exactly one.

use super::structure::{load, split_type, Column, ForeignKey, Index, Structure, Trigger};
use super::value::lit;
use super::{Db, Dialect, Pool};
use crate::db::{e, Msg, Res, UndoAction, UndoKind, Worker};
use std::collections::HashSet;
use std::time::Instant;

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
}

impl ColDef {
    pub fn new(name: &str, ty: &str) -> ColDef {
        ColDef { name: name.into(), ty: ty.into(), args: String::new(), unsigned: false, zerofill: false, nullable: true, default: Dv::Null, auto_increment: false, collation: None, on_update: None, comment: String::new(), srid: None }
    }

    /// What the structure read holds about a column. Generated, invisible and compressed columns and MariaDB
    /// column CHECKs carry attributes this definition does not write, so redefining them would lose them:
    /// those are refused.
    pub fn from_column(c: &Column) -> Result<ColDef, String> {
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
        Ok(ColDef { name: c.name.clone(), ty, args, unsigned, zerofill, nullable: c.nullable, default, auto_increment: lower.contains("auto_increment"), collation, on_update, comment: c.comment.clone(), srid: c.srid })
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
}

impl IndexDef {
    pub fn from_index(ix: &Index) -> IndexDef {
        let kind = if ix.name == "PRIMARY" {
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
        let functional = ix.cols.iter().any(|c| c.expr.is_some());
        IndexDef { name: ix.name.clone(), kind, cols: ix.cols.iter().map(|c| IdxCol { name: c.name.clone(), prefix: c.prefix, desc: c.desc }).collect(), comment: ix.comment.clone(), invisible: ix.invisible, functional }
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
        }
    }
}

/// Column and index names compare without case on both engines.
pub fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
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

fn is_geometry(ty: &str) -> bool {
    matches!(ty, "GEOMETRY" | "POINT" | "LINESTRING" | "POLYGON" | "MULTIPOINT" | "MULTILINESTRING" | "MULTIPOLYGON" | "GEOMETRYCOLLECTION")
}

fn is_binary(ty: &str) -> bool {
    matches!(ty, "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB")
}

/// `0x6162`, the way MySQL reports the default of a binary column.
fn is_hex_lit(v: &str) -> bool {
    v.strip_prefix("0x").is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `b'101'`, the way both engines report the default of a BIT column.
fn is_bit_lit(v: &str) -> bool {
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
pub fn values_text(vals: &[String]) -> String {
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

fn is_number(s: &str) -> bool {
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

/// Everything after the column name: `DECIMAL(12,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT '...'`.
pub fn definition(d: Dialect, m: Mode, c: &ColDef) -> Result<String, String> {
    check_ident(&c.name, "A column")?;
    let ty = c.ty.trim().to_ascii_uppercase();
    if ty.is_empty() || !ty.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || !ty.as_bytes()[0].is_ascii_alphabetic() {
        return Err(format!("{:?} is not a column type.", c.ty));
    }
    let mut s = ty.clone();
    let args = c.args.trim();
    if !args.is_empty() {
        let inner = if matches!(ty.as_str(), "ENUM" | "SET") {
            parse_values(args)?.iter().map(|v| lit(v, m.bs)).collect::<Vec<_>>().join(",")
        } else {
            let ok = args.split(',').count() <= 2 && args.split(',').all(|p| !p.trim().is_empty() && p.trim().bytes().all(|b| b.is_ascii_digit()));
            if !ok {
                return Err("The length is a number, or two numbers like 10,2.".into());
            }
            args.split(',').map(str::trim).collect::<Vec<_>>().join(",")
        };
        s.push_str(&format!("({inner})"));
    } else if matches!(ty.as_str(), "ENUM" | "SET") {
        return Err(format!("{ty} needs its values, like 'a','b'."));
    } else if matches!(ty.as_str(), "VARCHAR" | "VARBINARY") {
        return Err(format!("{ty} needs a length."));
    }
    if c.unsigned && is_numeric(&ty) {
        s.push_str(" UNSIGNED");
    }
    if c.zerofill && is_numeric(&ty) {
        s.push_str(" ZEROFILL");
    }
    if let (Some(n), Dialect::MySql, true) = (c.srid, d, is_geometry(&ty)) {
        s.push_str(&format!(" SRID {n}"));
    }
    if let (Some(coll), true) = (&c.collation, is_text(&ty)) {
        if coll.is_empty() || !coll.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(format!("{coll:?} is not a collation name."));
        }
        s.push_str(&format!(" CHARACTER SET {} COLLATE {coll}", coll.split('_').next().unwrap_or(coll)));
    }
    s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
    match &c.default {
        Dv::None => {}
        Dv::Null if c.nullable => s.push_str(" DEFAULT NULL"),
        Dv::Null => return Err("A NOT NULL column cannot default to NULL.".into()),
        Dv::Value(v) if is_numeric(&ty) && is_number(v.trim()) => s.push_str(&format!(" DEFAULT {}", v.trim())),
        // MySQL reports a binary default as hex, which read as text would store the digits.
        Dv::Value(v) if d == Dialect::MySql && is_binary(&ty) && is_hex_lit(v) => s.push_str(&format!(" DEFAULT {}", if v.len() == 2 { "X''" } else { v })),
        Dv::Value(v) if ty == "BIT" && is_bit_lit(v) => s.push_str(&format!(" DEFAULT {v}")),
        Dv::Value(v) if ty == "BIT" && !v.is_empty() && v.bytes().all(|b| b == b'0' || b == b'1') => s.push_str(&format!(" DEFAULT b'{v}'")),
        Dv::Value(v) => s.push_str(&format!(" DEFAULT {}", lit(v, m.bs))),
        Dv::Expr(x) => {
            check_expr(x, m)?;
            s.push_str(&format!(" DEFAULT {}", d.default_expr(x)));
        }
    }
    if c.auto_increment {
        if !is_integer(&ty) {
            return Err("Only an integer column can auto increment.".into());
        }
        s.push_str(" AUTO_INCREMENT");
    }
    if let Some(u) = c.on_update.as_deref().filter(|u| !u.is_empty()) {
        check_expr(u, m)?;
        s.push_str(&format!(" ON UPDATE {u}"));
    }
    if !c.comment.is_empty() {
        s.push_str(&format!(" COMMENT {}", lit(&c.comment, m.bs)));
    }
    Ok(s)
}

fn position(d: Dialect, p: &Pos) -> String {
    match p {
        Pos::Keep => String::new(),
        Pos::First => " FIRST".into(),
        Pos::After(c) => format!(" AFTER {}", d.quote_ident(c)),
    }
}

fn index_cols(d: Dialect, ix: &IndexDef) -> Result<String, String> {
    if ix.cols.is_empty() {
        return Err("An index needs at least one column.".into());
    }
    let parts: Vec<String> = ix.cols.iter().map(|c| format!("{}{}{}", d.quote_ident(&c.name), c.prefix.map_or(String::new(), |n| format!("({n})")), if c.desc { " DESC" } else { "" })).collect();
    Ok(parts.join(", "))
}

fn add_index(d: Dialect, m: Mode, ix: &IndexDef) -> Result<String, String> {
    if ix.functional {
        return Err(format!("Index {} has an expression part, which this editor cannot write.", ix.name));
    }
    let cols = index_cols(d, ix)?;
    if matches!(ix.kind, IndexKind::Fulltext | IndexKind::Spatial) && ix.cols.iter().any(|c| c.prefix.is_some() || c.desc) {
        return Err("A FULLTEXT or SPATIAL index has no prefix lengths or sort order.".into());
    }
    let comment = if ix.comment.is_empty() { String::new() } else { format!(" COMMENT {}", lit(&ix.comment, m.bs)) };
    if ix.kind == IndexKind::Primary {
        return Ok(format!("ADD PRIMARY KEY ({cols}){comment}"));
    }
    check_ident(&ix.name, "An index")?;
    let kind = match ix.kind {
        IndexKind::Unique => "UNIQUE ",
        IndexKind::Fulltext => "FULLTEXT ",
        IndexKind::Spatial => "SPATIAL ",
        _ => "",
    };
    let hidden = match (ix.invisible, d) {
        (false, _) => "",
        (true, Dialect::MySql) => " INVISIBLE",
        (true, Dialect::MariaDb) => " IGNORED",
    };
    Ok(format!("ADD {kind}INDEX {} ({cols}){comment}{hidden}", d.quote_ident(&ix.name)))
}

/// The actions a foreign key can take, as the server writes them.
pub const FK_ACTIONS: [&str; 4] = ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"];

fn add_fk(d: Dialect, k: &ForeignKey) -> Result<String, String> {
    check_ident(&k.name, "A foreign key")?;
    check_ident(&k.ref_table, "The referenced table")?;
    if k.cols.is_empty() || k.cols.len() != k.ref_cols.len() {
        return Err("Pick the same number of columns on both sides.".into());
    }
    let action = |a: &str| FK_ACTIONS.iter().find(|x| x.eq_ignore_ascii_case(a.trim())).copied().ok_or_else(|| format!("{a:?} is not a foreign key action."));
    let names = |v: &[String]| v.iter().map(|c| d.quote_ident(c)).collect::<Vec<_>>().join(", ");
    let target = if k.ref_db.is_empty() || k.ref_db == k.db { d.quote_ident(&k.ref_table) } else { d.qualified(&k.ref_db, &k.ref_table) };
    Ok(format!("ADD CONSTRAINT {}\n    FOREIGN KEY ({})\n    REFERENCES {} ({})\n    ON UPDATE {} ON DELETE {}", d.quote_ident(&k.name), names(&k.cols), target, names(&k.ref_cols), action(&k.on_update)?, action(&k.on_delete)?))
}

fn drop_index(d: Dialect, name: &str) -> String {
    if name == "PRIMARY" { "DROP PRIMARY KEY".into() } else { format!("DROP INDEX {}", d.quote_ident(name)) }
}

/// One clause of the ALTER, or two for a rename the server cannot do directly.
fn clause(d: Dialect, m: Mode, can_rename: bool, c: &Change) -> Result<Vec<String>, String> {
    Ok(match c {
        Change::Add { def, pos } => vec![format!("ADD COLUMN {} {}{}", d.quote_ident(&def.name), definition(d, m, def)?, position(d, pos))],
        Change::Modify { from, def, pos } if *from == def.name => vec![format!("MODIFY {} {}{}", d.quote_ident(from), definition(d, m, def)?, position(d, pos))],
        Change::Modify { from, def, pos } => vec![format!("CHANGE COLUMN {} {} {}{}", d.quote_ident(from), d.quote_ident(&def.name), definition(d, m, def)?, position(d, pos))],
        Change::Drop(n) => vec![format!("DROP COLUMN {}", d.quote_ident(n))],
        Change::AddIndex(ix) => vec![add_index(d, m, ix)?],
        Change::DropIndex(n) => vec![drop_index(d, n)],
        Change::AddFk(k) => vec![add_fk(d, k)?],
        Change::DropFk(n) => vec![format!("DROP FOREIGN KEY {}", d.quote_ident(n))],
        Change::RenameIndex { from, to, def } => {
            check_ident(to, "An index")?;
            if can_rename {
                vec![format!("RENAME INDEX {} TO {}", d.quote_ident(from), d.quote_ident(to))]
            } else {
                vec![drop_index(d, from), add_index(d, m, &IndexDef { name: to.clone(), ..def.clone() })?]
            }
        }
    })
}

/// The statement for the staged changes, one clause per line. The table is unqualified: the writer's
/// session is switched to the schema first, which is what the preview shows.
pub fn alter(d: Dialect, version: &str, m: Mode, table: &str, changes: &[Change]) -> Result<String, String> {
    if changes.is_empty() {
        return Err("Nothing is staged.".into());
    }
    let can = d.can_rename_index(version);
    let mut lines = Vec::new();
    for c in changes {
        lines.extend(clause(d, m, can, c)?);
    }
    Ok(format!("ALTER TABLE {}\n  {}", d.quote_ident(table), lines.join(",\n  ")))
}

/// The table as the changes leave it, tracked while their inverses are collected. A column whose definition
/// cannot be read back keeps the reason.
#[derive(Clone)]
struct Sim {
    cols: Vec<(String, Result<ColDef, String>)>,
    idx: Vec<IndexDef>,
    fks: Vec<ForeignKey>,
}

impl Sim {
    fn of(st: &Structure) -> Sim {
        Sim { cols: st.columns.iter().map(|c| (c.name.clone(), ColDef::in_table(st, c))).collect(), idx: st.indexes.iter().map(IndexDef::from_index).collect(), fks: st.foreign_keys.clone() }
    }

    /// An index the server can use for a key on these columns: it starts with them, in order, whole.
    fn serves(&self, cols: &[String]) -> bool {
        self.idx.iter().any(|ix| matches!(ix.kind, IndexKind::Index | IndexKind::Unique | IndexKind::Primary) && ix.cols.len() >= cols.len() && ix.cols.iter().zip(cols).all(|(a, b)| a.prefix.is_none() && same_name(&a.name, b)))
    }

    fn fk_at(&self, name: &str) -> Option<usize> {
        self.fks.iter().position(|k| same_name(&k.name, name))
    }

    /// The index to drop along with a key: one named like the key on exactly its columns is what the server
    /// (or this app) made for it. Another key that relies on it keeps it.
    fn backing_index(&self, name: &str) -> Option<String> {
        let fk = &self.fks[self.fk_at(name)?];
        let ix = self.idx.iter().find(|ix| ix.kind == IndexKind::Index && same_name(&ix.name, &fk.name) && ix.cols.len() == fk.cols.len() && ix.cols.iter().zip(&fk.cols).all(|(a, b)| a.prefix.is_none() && !a.desc && same_name(&a.name, b)))?;
        let shared = self.fks.iter().any(|o| !same_name(&o.name, &fk.name) && o.cols.len() <= ix.cols.len() && ix.cols.iter().zip(&o.cols).all(|(a, b)| same_name(&a.name, b)));
        (!shared).then(|| ix.name.clone())
    }

    fn at(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|(n, _)| same_name(n, name))
    }

    /// Where a column is now, as the clause that puts it back there.
    fn pos_of(&self, i: usize) -> Pos {
        if i == 0 { Pos::First } else { Pos::After(self.cols[i - 1].0.clone()) }
    }

    fn insert(&mut self, col: (String, Result<ColDef, String>), pos: &Pos) {
        let at = match pos {
            Pos::Keep => self.cols.len(),
            Pos::First => 0,
            Pos::After(c) => self.at(c).map_or(self.cols.len(), |i| i + 1),
        };
        self.cols.insert(at.min(self.cols.len()), col);
    }

    /// The server renames a column in the indexes and foreign keys that hold it.
    fn rename_in_indexes(&mut self, from: &str, to: &str) {
        for ix in &mut self.idx {
            for c in ix.cols.iter_mut().filter(|c| same_name(&c.name, from)) {
                c.name = to.into();
            }
        }
        for k in &mut self.fks {
            for c in k.cols.iter_mut().filter(|c| same_name(c, from)) {
                *c = to.into();
            }
        }
    }

    /// Applies one change and returns the changes that undo it, in the order to run them.
    fn apply(&mut self, c: &Change) -> Result<Vec<Change>, String> {
        match c {
            Change::Add { def, pos } => {
                self.insert((def.name.clone(), Ok(def.clone())), pos);
                Ok(vec![Change::Drop(def.name.clone())])
            }
            Change::Modify { from, def, pos } => {
                let i = self.at(from).ok_or_else(|| format!("Column {from} is not in the table."))?;
                let old_pos = self.pos_of(i);
                let (_, old) = self.cols.remove(i);
                if *pos == Pos::Keep {
                    self.cols.insert(i, (def.name.clone(), Ok(def.clone())));
                } else {
                    self.insert((def.name.clone(), Ok(def.clone())), pos);
                }
                if from != &def.name {
                    self.rename_in_indexes(from, &def.name);
                }
                let back_pos = if *pos == Pos::Keep { Pos::Keep } else { old_pos };
                Ok(vec![Change::Modify { from: def.name.clone(), def: old?, pos: back_pos }])
            }
            Change::Drop(name) => {
                let i = self.at(name).ok_or_else(|| format!("Column {name} is not in the table."))?;
                let pos = self.pos_of(i);
                let (_, old) = self.cols.remove(i);
                // The server shrinks every index that holds the column and drops those left empty, and undo has to
                // bring them back as they were.
                let mut back = vec![Change::Add { def: old?, pos }];
                let mut keep = Vec::new();
                for mut ix in std::mem::take(&mut self.idx) {
                    if !ix.cols.iter().any(|c| same_name(&c.name, name)) {
                        keep.push(ix);
                        continue;
                    }
                    let whole = ix.clone();
                    ix.cols.retain(|c| !same_name(&c.name, name));
                    if !ix.cols.is_empty() {
                        back.push(Change::DropIndex(ix.name.clone()));
                        keep.push(ix);
                    }
                    back.push(Change::AddIndex(whole));
                }
                self.idx = keep;
                Ok(back)
            }
            Change::AddIndex(def) => {
                self.idx.push(def.clone());
                Ok(vec![Change::DropIndex(if def.kind == IndexKind::Primary { "PRIMARY".into() } else { def.name.clone() })])
            }
            Change::DropIndex(name) => {
                let i = self.idx.iter().position(|x| same_name(&x.name, name) || (name == "PRIMARY" && x.kind == IndexKind::Primary)).ok_or_else(|| format!("Index {name} is not on the table."))?;
                if self.idx[i].functional {
                    return Err(format!("Index {name} has an expression part, which cannot be read back to add it again."));
                }
                Ok(vec![Change::AddIndex(self.idx.remove(i))])
            }
            Change::RenameIndex { from, to, def } => {
                let i = self.idx.iter().position(|x| same_name(&x.name, from)).ok_or_else(|| format!("Index {from} is not on the table."))?;
                self.idx[i].name = to.clone();
                Ok(vec![Change::RenameIndex { from: to.clone(), to: from.clone(), def: IndexDef { name: to.clone(), ..def.clone() } }])
            }
            Change::AddFk(k) => {
                if self.fk_at(&k.name).is_some() {
                    return Err(format!("The table already has a foreign key called {}.", k.name));
                }
                self.fks.push(k.clone());
                Ok(vec![Change::DropFk(k.name.clone())])
            }
            Change::DropFk(name) => {
                let i = self.fk_at(name).ok_or_else(|| format!("Foreign key {name} is not on the table."))?;
                Ok(vec![Change::AddFk(self.fks.remove(i))])
            }
        }
    }
}

/// The column names after the changes, for the position lists and for moving a column.
pub fn columns_after(st: &Structure, changes: &[Change]) -> Vec<String> {
    let mut sim = Sim::of(st);
    for c in changes {
        let _ = sim.apply(c);
    }
    sim.cols.into_iter().map(|(n, _)| n).collect()
}

/// The index names after the changes, for the duplicate check of the index form.
pub fn indexes_after(st: &Structure, changes: &[Change]) -> Vec<String> {
    let mut sim = Sim::of(st);
    for c in changes {
        let _ = sim.apply(c);
    }
    sim.idx.into_iter().map(|i| i.name).collect()
}

/// The name a column ends up with once `later` ran.
fn final_name(name: &str, later: &[Change]) -> String {
    later.iter().fold(name.to_string(), |n, c| match c {
        Change::Modify { from, def, .. } if same_name(from, &n) => def.name.clone(),
        _ => n,
    })
}

/// The changes with the index edits the engine will not do by itself. MariaDB refuses to drop a column that an
/// index holds, so each such index is dropped first and added again without the column (or left out when the
/// column was its only one). A new foreign key gets an index of its own when none serves its columns, written
/// out so the preview shows it, and dropping a key takes the index made for it along.
pub fn expand(d: Dialect, st: &Structure, changes: &[Change]) -> Vec<Change> {
    let mut sim = Sim::of(st);
    let mut out = Vec::new();
    for (k, c) in changes.iter().enumerate() {
        match c {
            Change::Drop(col) if !d.drops_index_with_column() => {
                for ix in sim.idx.iter().filter(|ix| ix.cols.iter().any(|c| same_name(&c.name, col))) {
                    out.push(Change::DropIndex(if ix.kind == IndexKind::Primary { "PRIMARY".into() } else { ix.name.clone() }));
                    let mut rest = ix.clone();
                    rest.cols.retain(|c| !same_name(&c.name, col));
                    // The server reads the columns of an added index by the names the whole ALTER leaves, so a column a
                    // later change renames goes in by its new name.
                    for ic in &mut rest.cols {
                        ic.name = final_name(&ic.name, &changes[k + 1..]);
                    }
                    if !rest.cols.is_empty() {
                        out.push(Change::AddIndex(rest));
                    }
                }
            }
            Change::AddFk(fk) if !sim.serves(&fk.cols) => {
                let taken = |n: &str| sim.idx.iter().any(|x| same_name(&x.name, n));
                let name = if taken(&fk.name) { format!("{}_idx", fk.name) } else { fk.name.clone() };
                let ix = IndexDef { name, kind: IndexKind::Index, cols: fk.cols.iter().map(|c| IdxCol { name: c.clone(), prefix: None, desc: false }).collect(), ..Default::default() };
                let _ = sim.apply(&Change::AddIndex(ix.clone()));
                out.push(Change::AddIndex(ix));
            }
            _ => {}
        }
        let backing = if let Change::DropFk(n) = c { sim.backing_index(n) } else { None };
        let _ = sim.apply(c);
        out.push(c.clone());
        if let Some(ix) = backing {
            let _ = sim.apply(&Change::DropIndex(ix.clone()));
            out.push(Change::DropIndex(ix));
        }
    }
    out
}

/// What an applied structure change needs to be undone: the statements, what running them would lose, and
/// what the table looked like right after the change, which must still hold when the undo runs.
#[derive(Clone, Debug, PartialEq)]
pub struct AlterUndo {
    /// One ALTER per inverse step, run in order. One statement could not do it: the server reads the columns of
    /// an added index by the names the statement leaves and drops columns before it places them, so a step
    /// that names a column the next one renames or drops would fail.
    pub steps: Vec<String>,
    /// What the change did, for the Recent changes list.
    pub label: String,
    /// What running the undo loses (a column it drops, a type it converts back), for its typed confirmation.
    pub loss: Vec<String>,
    /// The sql_mode the steps' literals are written for.
    pub mode: Mode,
    /// The columns, indexes and foreign keys the steps touch.
    pub cols: Vec<String>,
    pub idx: Vec<String>,
    pub fks: Vec<String>,
    /// `snapshot` of those right after the change ran, filled by the worker.
    pub expect: Vec<(String, Option<String>)>,
}

/// The staged changes as they run, and their undo or why there is none. The preview and Apply both use it, so
/// what is shown is what runs.
pub struct Plan {
    pub sql: String,
    pub undo: Result<AlterUndo, String>,
}

pub fn plan(d: Dialect, version: &str, m: Mode, table: &str, st: &Structure, staged: &[Change]) -> Result<Plan, String> {
    let changes = expand(d, st, staged);
    let sql = alter(d, version, m, table, &changes)?;
    Ok(Plan { sql, undo: undo_of(d, version, m, table, st, &changes) })
}

fn undo_of(d: Dialect, version: &str, m: Mode, table: &str, before: &Structure, changes: &[Change]) -> Result<AlterUndo, String> {
    let mut sim = Sim::of(before);
    let mut groups = Vec::new();
    for c in changes {
        groups.push(sim.apply(c)?);
    }
    groups.reverse();
    let steps = groups.iter().map(|g| alter(d, version, m, table, g)).collect::<Result<Vec<_>, _>>()?;
    let loss = undo_loss(&sim, &groups);
    let (cols, idx, fks) = touched(&groups);
    Ok(AlterUndo { steps, label: String::new(), loss, mode: m, cols, idx, fks, expect: Vec::new() })
}

/// The statements that put the table back, from the structure read before the changes ran. Err says why
/// there are none.
#[cfg(test)]
pub fn inverse(d: Dialect, version: &str, m: Mode, table: &str, before: &Structure, changes: &[Change]) -> Result<Vec<String>, String> {
    undo_of(d, version, m, table, before, changes).map(|u| u.steps)
}

/// A new definition converts the values a column holds.
fn converts(old: &ColDef, new: &ColDef) -> bool {
    old.ty != new.ty || old.args != new.args || old.unsigned != new.unsigned || (old.nullable && !new.nullable)
}

fn type_text(c: &ColDef) -> String {
    format!("{}{}{}", c.ty, if c.args.is_empty() { String::new() } else { format!("({})", c.args) }, if c.unsigned { " UNSIGNED" } else { "" })
}

/// What the undo steps destroy, run on the table the change left.
fn undo_loss(after: &Sim, groups: &[Vec<Change>]) -> Vec<String> {
    let mut sim = after.clone();
    let mut out = Vec::new();
    for c in groups.iter().flatten() {
        match c {
            Change::Drop(n) => out.push(format!("Undo drops column {n} and every value it holds.")),
            Change::Modify { from, def, .. } => {
                if let Some(Ok(cur)) = sim.at(from).map(|i| &sim.cols[i].1) {
                    if converts(cur, def) {
                        out.push(format!("Undo converts {from} back to {}, which can cut or reject the values it holds now.", type_text(def)));
                    }
                }
            }
            _ => {}
        }
        let _ = sim.apply(c);
    }
    out
}

/// The column, index and foreign key names the undo steps read or write, with no repeats.
fn touched(groups: &[Vec<Change>]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut cols, mut idx, mut fks): (Vec<String>, Vec<String>, Vec<String>) = (Vec::new(), Vec::new(), Vec::new());
    fn add(v: &mut Vec<String>, n: &str) {
        if !v.iter().any(|x| same_name(x, n)) {
            v.push(n.to_string());
        }
    }
    for c in groups.iter().flatten() {
        let pos = match c {
            Change::Add { def, pos } => {
                add(&mut cols, &def.name);
                Some(pos)
            }
            Change::Modify { from, def, pos } => {
                add(&mut cols, from);
                add(&mut cols, &def.name);
                Some(pos)
            }
            Change::Drop(n) => {
                add(&mut cols, n);
                None
            }
            Change::AddIndex(ix) => {
                add(&mut idx, &ix.name);
                for ic in &ix.cols {
                    add(&mut cols, &ic.name);
                }
                None
            }
            Change::DropIndex(n) => {
                add(&mut idx, n);
                None
            }
            Change::RenameIndex { from, to, .. } => {
                add(&mut idx, from);
                add(&mut idx, to);
                None
            }
            Change::AddFk(k) => {
                add(&mut fks, &k.name);
                for c in &k.cols {
                    add(&mut cols, c);
                }
                None
            }
            Change::DropFk(n) => {
                add(&mut fks, n);
                None
            }
        };
        if let Some(Pos::After(n)) = pos {
            add(&mut cols, n);
        }
    }
    (cols, idx, fks)
}

/// How the server shows each named column, index and foreign key, or None when it is not there. Two snapshots are
/// equal when nothing an undo relies on changed in between. Every part is compared exactly, table names too: both
/// snapshots come from the same server, which spells a name the same way each time whatever lower_case_table_names is.
pub fn snapshot(st: &Structure, cols: &[String], idx: &[String], fks: &[String]) -> Vec<(String, Option<String>)> {
    let col = |n: &String| {
        let c = st.columns.iter().find(|c| same_name(&c.name, n));
        (format!("column {n}"), c.map(|c| format!("{}|{}|{}|{:?}|{}|{}|{:?}|{}|{:?}|{:?}", c.name, c.full_type, c.nullable, c.default, c.default_expr, c.extra, c.collation, c.comment, c.srid, c.check)))
    };
    let index = |n: &String| {
        let i = st.indexes.iter().find(|i| same_name(&i.name, n));
        (format!("index {n}"), i.map(|i| format!("{}|{}|{}|{}|{}|{}", i.name, i.unique, i.kind, i.comment, i.invisible, i.cols.iter().map(|c| format!("{}:{:?}:{}:{:?}", c.name, c.prefix, c.desc, c.expr)).collect::<Vec<_>>().join(","))))
    };
    let fk = |n: &String| {
        let k = st.foreign_keys.iter().find(|k| same_name(&k.name, n));
        (format!("foreign key {n}"), k.map(|k| format!("{}|{}|{}|{}|{}|{}|{}", k.name, k.cols.join(","), k.ref_db, k.ref_table, k.ref_cols.join(","), k.on_update, k.on_delete)))
    };
    cols.iter().map(col).chain(idx.iter().map(index)).chain(fks.iter().map(fk)).collect()
}

/// What the staged changes cost that undo cannot give back. Shown in the preview.
pub fn warnings(before: &Structure, changes: &[Change]) -> Vec<String> {
    let mut out = Vec::new();
    let mut fk_sim = Sim::of(before);
    let sim = Sim::of(before);
    for c in changes {
        if let Change::DropFk(n) = c {
            if let Some(ix) = fk_sim.backing_index(n) {
                out.push(format!("Dropping {n} also drops the index {ix}, which was made for it. Undo brings both back."));
            }
        }
        let _ = fk_sim.apply(c);
        match c {
            Change::Drop(n) => out.push(format!("Dropping {n} deletes its values for good. Undo brings the column back empty.")),
            Change::Modify { from, def, .. } => {
                if let Some(Ok(old)) = sim.at(from).map(|i| &sim.cols[i].1) {
                    if converts(old, def) {
                        out.push(format!("Changing {from} converts its values and can cut or reject some. Undo restores the definition, not the values."));
                    }
                }
            }
            Change::DropIndex(n) if n == "PRIMARY" => out.push("Dropping the primary key makes the rows read-only here until the table has a key again.".into()),
            _ => {}
        }
    }
    out
}

/// True when the changes lose data that undo cannot restore (so Apply asks to be confirmed).
pub fn drops_data(changes: &[Change]) -> bool {
    changes.iter().any(|c| matches!(c, Change::Drop(_)))
}

/// Why an index of this kind cannot be built on these columns, as (name, type) pairs.
pub fn index_problem(kind: IndexKind, cols: &[(String, String)]) -> Option<String> {
    let ty = |c: &(String, String)| split_type(&c.1).0;
    match kind {
        IndexKind::Fulltext if cols.iter().any(|c| !matches!(ty(c).as_str(), "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT")) => Some("FULLTEXT works on text columns only.".into()),
        IndexKind::Spatial if cols.len() != 1 || !is_geometry(&ty(&cols[0])) => Some("SPATIAL takes one geometry column.".into()),
        _ => None,
    }
}

/// Dedupes names for the "Duplicate" action: `name_copy`, `name_copy2`, and so on.
pub fn free_name(taken: &[String], base: &str) -> String {
    let set: HashSet<String> = taken.iter().map(|t| t.to_lowercase()).collect();
    let first = format!("{base}_copy");
    if !set.contains(&first.to_lowercase()) {
        return first;
    }
    (2..).map(|n| format!("{first}{n}")).find(|n| !set.contains(&n.to_lowercase())).unwrap_or(first)
}

/// Why a column pair cannot be the two sides of a foreign key, in the words of the person adding it. The
/// server's own answer ("incompatible") names no column, so the rules it enforces are checked first.
pub fn fk_pair_problem(child_table: &str, child: &Column, parent_table: &str, parent: &Column) -> Option<String> {
    let (c, p) = (format!("{child_table}.{}", child.name), format!("{parent_table}.{}", parent.name));
    let (ct, cargs, cu) = split_type(&child.full_type);
    let (pt, pargs, pu) = split_type(&parent.full_type);
    let show = |col: &Column| col.full_type.to_ascii_uppercase();
    #[derive(PartialEq)]
    enum Fam {
        Int,
        Decimal,
        Float,
        Double,
        Text,
        Bin,
        Other(String),
    }
    let fam = |t: &str| -> Option<Fam> {
        Some(match t {
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" => Fam::Int,
            "DECIMAL" | "NUMERIC" => Fam::Decimal,
            "FLOAT" => Fam::Float,
            "DOUBLE" | "REAL" => Fam::Double,
            "CHAR" | "VARCHAR" => Fam::Text,
            "BINARY" | "VARBINARY" => Fam::Bin,
            "DATE" | "DATETIME" | "TIMESTAMP" | "TIME" | "YEAR" | "BIT" | "ENUM" | "SET" => Fam::Other(t.to_string()),
            _ => return None,
        })
    };
    for (name, t, col) in [(&c, &ct, child), (&p, &pt, parent)] {
        if fam(t).is_none() {
            return Some(format!("{name} is {}, which cannot be part of a foreign key.", show(col)));
        }
    }
    let (fc, fp) = (fam(&ct)?, fam(&pt)?);
    if fc != fp {
        return Some(format!("{c} is {} but {p} is {}. A foreign key needs the same kind of type on both sides.", show(child), show(parent)));
    }
    let size = |t: &str| if t == "INTEGER" { "INT".to_string() } else { t.to_string() };
    match fc {
        Fam::Int if size(&ct) != size(&pt) || cu != pu => Some(format!("{c} is {} but {p} is {}. Integer columns must have the same size and the same signedness.", show(child), show(parent))),
        Fam::Decimal | Fam::Float | Fam::Double if cargs != pargs || cu != pu || (fc == Fam::Float && ct != pt) => Some(format!("{c} is {} but {p} is {}. Number columns must match exactly.", show(child), show(parent))),
        Fam::Other(_) if child.full_type.to_ascii_lowercase() != parent.full_type.to_ascii_lowercase() => Some(format!("{c} is {} but {p} is {}. These types must match exactly.", show(child), show(parent))),
        Fam::Text => {
            let (a, b) = (child.collation.as_deref().map(str::to_ascii_lowercase), parent.collation.as_deref().map(str::to_ascii_lowercase));
            (a != b).then(|| format!("{c} uses {} but {p} uses {}. Give both columns the same collation.", child.collation.as_deref().unwrap_or("the default collation"), parent.collation.as_deref().unwrap_or("the default collation")))
        }
        _ => None,
    }
}

/// The first problem with a whole key, before the server is asked: column pairs that do not fit, SET NULL on a
/// column that cannot be NULL, and a referenced side with no index to lean on.
pub fn fk_problem(own: &Structure, table: &str, parent: &Structure, fk: &ForeignKey) -> Option<String> {
    let find = |st: &Structure, n: &str| st.columns.iter().find(|c| c.name.eq_ignore_ascii_case(n)).cloned();
    for (a, b) in fk.cols.iter().zip(&fk.ref_cols) {
        let (Some(ca), Some(cb)) = (find(own, a), find(parent, b)) else { return Some("A picked column is not in its table any more.".into()) };
        if let Some(p) = fk_pair_problem(table, &ca, &fk.ref_table, &cb) {
            return Some(p);
        }
        let set_null = fk.on_delete.eq_ignore_ascii_case("SET NULL") || fk.on_update.eq_ignore_ascii_case("SET NULL");
        if set_null && !ca.nullable {
            return Some(format!("SET NULL needs a column that allows NULL, and {table}.{a} does not."));
        }
    }
    let led = parent.indexes.iter().any(|ix| ix.cols.len() >= fk.ref_cols.len() && ix.cols.iter().zip(&fk.ref_cols).all(|(x, y)| x.prefix.is_none() && x.name.eq_ignore_ascii_case(y)));
    (!led).then(|| format!("{} has no index that starts with ({}). Add an index there first.", fk.ref_table, fk.ref_cols.join(", ")))
}

/// Counts the rows of the table whose key points at nothing, which is what makes ADD FOREIGN KEY fail.
pub fn orphans_sql(d: Dialect, schema: &str, table: &str, fk: &ForeignKey) -> String {
    let q = |n: &str| d.quote_ident(n);
    let parent = d.qualified(if fk.ref_db.is_empty() { schema } else { &fk.ref_db }, &fk.ref_table);
    let set = |t: &str, cols: &[String]| cols.iter().map(|c| format!("{t}.{}", q(c))).collect::<Vec<_>>();
    let present = set("c", &fk.cols).iter().map(|c| format!("{c} IS NOT NULL")).collect::<Vec<_>>().join(" AND ");
    let same = set("c", &fk.cols).iter().zip(set("p", &fk.ref_cols)).map(|(a, b)| format!("{b} = {a}")).collect::<Vec<_>>().join(" AND ");
    format!("SELECT COUNT(*) AS n FROM {} c WHERE {present} AND NOT EXISTS (SELECT 1 FROM {parent} p WHERE {same})", d.qualified(schema, table))
}

/// A trigger as the editor holds it. The body is the user's own SQL, written after FOR EACH ROW as typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrigDef {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub body: String,
}

/// The body without its trailing `;`. A body that is not BEGIN ... END must be one statement, so a second
/// statement typed after a `;` is refused instead of running.
pub fn check_body(body: &str, bs: bool, ansi: bool) -> Result<String, String> {
    use super::lex::{tokens_with, Tok};
    let text = body.trim();
    let text = text.strip_suffix(';').unwrap_or(text).trim_end();
    let toks = tokens_with(text, bs, ansi);
    let code: Vec<_> = toks.iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    let Some((first, r)) = code.first() else { return Err("The trigger body is empty.".into()) };
    if *first == Tok::Word && text[r.clone()].eq_ignore_ascii_case("BEGIN") {
        return Ok(text.to_string());
    }
    if code.iter().any(|(t, r)| *t == Tok::Punct && &text[r.clone()] == ";") {
        return Err("A body of several statements goes inside BEGIN ... END.".into());
    }
    Ok(text.to_string())
}

fn trigger_head(d: Dialect, bs: bool, ansi: bool, table: &str, t: &TrigDef, replace: bool) -> Result<String, String> {
    check_ident(&t.name, "A trigger")?;
    let timing = t.timing.trim().to_ascii_uppercase();
    let event = t.event.trim().to_ascii_uppercase();
    if !matches!(timing.as_str(), "BEFORE" | "AFTER") {
        return Err("Timing is BEFORE or AFTER.".into());
    }
    if !matches!(event.as_str(), "INSERT" | "UPDATE" | "DELETE") {
        return Err("The event is INSERT, UPDATE or DELETE.".into());
    }
    let body = check_body(&t.body, bs, ansi)?;
    let head = if replace && d == Dialect::MariaDb { "CREATE OR REPLACE TRIGGER" } else { "CREATE TRIGGER" };
    Ok(format!("{head} {} {timing} {event} ON {}\nFOR EACH ROW\n{body}", d.quote_ident(&t.name), d.quote_ident(table)))
}

/// The statements that save a trigger, each run on its own: one CREATE for a new trigger; for an edit MariaDB
/// replaces in place and MySQL, which cannot, drops first.
pub fn trigger_steps(d: Dialect, bs: bool, ansi: bool, table: &str, t: &TrigDef, exists: bool) -> Result<Vec<String>, String> {
    let create = trigger_head(d, bs, ansi, table, t, exists)?;
    Ok(if exists && d == Dialect::MySql { vec![drop_trigger(d, &t.name, false), create] } else { vec![create] })
}

pub fn drop_trigger(d: Dialect, name: &str, if_exists: bool) -> String {
    format!("DROP TRIGGER {}{}", if if_exists { "IF EXISTS " } else { "" }, d.quote_ident(name))
}

/// The statements as the preview shows them.
pub fn script(steps: &[String]) -> String {
    steps.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n\n")
}

/// What a trigger undo expects to find before it runs, so it never overwrites a change made since: the trigger as
/// the change left it, or None for one that must be absent.
#[derive(Clone, Debug)]
pub struct Guard {
    pub name: String,
    pub present: Option<Trigger>,
}

impl Guard {
    /// Err says what changed.
    pub fn check(&self, st: &Structure) -> Result<(), String> {
        let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        let live = st.triggers.iter().find(|t| same(&t.name, &self.name));
        match (live, &self.present) {
            (Some(l), Some(w)) if same(&l.timing, &w.timing) && same(&l.event, &w.event) && l.body.trim() == w.body.trim() => Ok(()),
            (None, None) => Ok(()),
            _ => Err(format!("The trigger {} is not as this change left it, so undo would overwrite a newer change. Undo it by hand.", self.name)),
        }
    }
}

/// A writable connection whose session reads text the way the statements were written for. When the server's
/// sql_mode changed since, nothing runs and the error carries the mode now in force.
pub(super) async fn session(db: &Db, schema: &str, read_only: bool, m: Mode) -> Result<sqlx::MySqlConnection, (String, Option<String>)> {
    let mut c = super::write::writer(db, schema, read_only).await.map_err(|x| (x, None))?;
    let now: String = sqlx::query_scalar("SELECT @@SESSION.sql_mode").fetch_one(&mut c).await.map_err(|x| (e(x), None))?;
    if Mode::of(&now) != m {
        return Err((format!("Nothing ran: the server's sql_mode is now {now:?}, which reads quotes or backslashes differently from when the statement was written."), Some(now)));
    }
    Ok(c)
}

/// One statement, prepared, so the server parses exactly one whatever the text holds.
pub(super) async fn exec(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<()> {
    sqlx::query(sql).persistent(false).execute(&mut *c).await.map_err(e)?;
    Ok(())
}

fn undo_action(conn: u64, schema: &str, table: &str, u: AlterUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { cols: Vec::new(), undo: super::write::SqlUndo::Alter(Box::new(u)) } }
}

/// Runs a recorded undo after checking the table is still as the change left it. The answer is the same as for
/// applying one; the action comes back for another try only when nothing ran.
pub(super) async fn undo(db: &Db, a: UndoAction, u: AlterUndo, read_only: bool) -> Msg {
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let (mut ran, mut seen) = (0, None);
    let res: Res<String> = async {
        let st = load(db, &schema, &table).await?;
        let now = snapshot(&st, &u.cols, &u.idx, &u.fks);
        if now.len() != u.expect.len() {
            return Err("Undo refused: the table could not be compared with how the change left it.".to_string());
        }
        if let Some((what, _)) = u.expect.iter().zip(&now).find(|(a, b)| a != b).map(|(a, _)| a) {
            return Err(format!("Undo refused: {what} changed after this change ran, so undoing it now would not put the table back."));
        }
        let mut c = match session(db, &schema, read_only, u.mode).await {
            Ok(c) => c,
            Err((why, now)) => {
                seen = now;
                return Err(why);
            }
        };
        for s in &u.steps {
            exec(&mut c, s).await.map_err(|err| if ran == 0 { err } else { format!("Undo stopped at step {} of {}: {err}. The steps before it ran, so the table is only partly put back.", ran + 1, u.steps.len()) })?;
            ran += 1;
        }
        Ok("Undid the structure change".to_string())
    }
    .await;
    let keep = (res.is_err() && ran == 0).then(|| {
        let mut k = undo_action(conn, &schema, &table, u);
        // Keeps the age of the original so a failed undo does not live longer.
        k.at = at;
        k
    });
    Msg::SqlAltered { conn, schema, table, res, undo: keep, sql_mode: seen }
}

impl Worker {
    /// Runs one ALTER TABLE on a writable connection of its own, written for the sql_mode `m`. `undo` reverses it
    /// when there is one; it is handed back with the answer along with how the table looked right after.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_alter(&self, conn: u64, db: Db, schema: String, table: String, sql: String, m: Mode, undo: Option<AlterUndo>, read_only: bool, done: String) {
        self.spawn(async move {
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
            Msg::SqlAltered { conn, schema, table, res: res.map(|_| done), undo, sql_mode: seen }
        });
    }
}

fn ddl_undo(conn: u64, schema: &str, table: &str, steps: Vec<String>, guards: Vec<Guard>, label: String) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { cols: Vec::new(), undo: super::write::SqlUndo::Ddl { steps, guards, label } } }
}

/// Runs the statements one at a time on one connection, each as a call of its own.
async fn run_steps(c: &mut sqlx::MySqlConnection, steps: &[String]) -> Res<()> {
    for s in steps {
        sqlx::Executor::execute(&mut *c, s.as_str()).await.map_err(e)?;
    }
    Ok(())
}

/// Runs a recorded undo of a trigger change. The live structure is read first, and anything that is not as the
/// change left it stops the undo.
pub(super) async fn undo_ddl(db: &Db, a: UndoAction, steps: Vec<String>, guards: Vec<Guard>, label: String, read_only: bool) -> Msg {
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let res = async {
        let mut c = super::write::writer(db, &schema, read_only).await?;
        let st = load(db, &schema, &table).await?;
        for g in &guards {
            g.check(&st)?;
        }
        run_steps(&mut c, &steps).await
    }
    .await
    .map(|_| "Undid the change".to_string());
    let keep = res.is_err().then(|| UndoAction { at, ..ddl_undo(conn, &schema, &table, steps, guards, label) });
    Msg::SqlAltered { conn, schema, table, res, undo: keep, sql_mode: None }
}

/// Reads the trigger being replaced, runs the statements and reads the trigger as it is now. A named fn rather than
/// an async block, which the compiler cannot prove Send around a borrowed connection.
async fn trigger_job(db: &Db, schema: &str, table: &str, name: &str, steps: &[String], existed: bool, read_only: bool) -> Res<(Option<String>, Option<Trigger>)> {
    let mut c = super::write::writer(db, schema, read_only).await?;
    // Read from the pool with the schema spelled out, since the writer's own connection trips the borrow checker here.
    let Pool::My(pool) = &db.pool;
    let show = format!("SHOW CREATE TRIGGER {}", db.dialect.qualified(schema, name));
    let old = sqlx::raw_sql(&show).fetch_one(pool).await.ok().and_then(|r| super::worker::raw(&r, 2).map(|b| String::from_utf8_lossy(b).into_owned()));
    if existed && old.is_none() {
        return Err(format!("Could not read the current definition of {name}, so it was left as it is."));
    }
    for (i, s) in steps.iter().enumerate() {
        if let Err(err) = sqlx::Executor::execute(&mut c, s.as_str()).await {
            let err = e(err);
            // Only a step after the DROP can leave the trigger missing.
            return Err(match (i, &old) {
                (1.., Some(o)) => match sqlx::Executor::execute(&mut c, o.as_str()).await {
                    Ok(_) => format!("{err}. The previous trigger was put back."),
                    Err(back) => format!("{err}. The previous trigger could not be put back ({}). Its definition:\n{o}", e(back)),
                },
                _ => err,
            });
        }
    }
    let live = load(db, schema, table).await.ok().and_then(|st| st.triggers.into_iter().find(|t| t.name.eq_ignore_ascii_case(name)));
    Ok((old, live))
}

impl Worker {
    /// Saves or drops a trigger. The definition it replaces is read first with SHOW CREATE TRIGGER: it is put
    /// back when the new one is refused (MySQL has to drop before it creates) and it is what undo restores.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_trigger(&self, conn: u64, db: Db, schema: String, table: String, name: String, steps: Vec<String>, existed: bool, dropping: bool, read_only: bool, done: String) {
        self.spawn(async move {
            let res = trigger_job(&db, &schema, &table, &name, &steps, existed, read_only).await;
            let (res, undo) = match res {
                Ok((old, live)) => {
                    let d = db.dialect;
                    let (back, present, label) = match (&old, dropping) {
                        (Some(o), true) => (Some(vec![o.clone()]), None, format!("dropped trigger {name}")),
                        (Some(o), false) => (Some(vec![drop_trigger(d, &name, true), o.clone()]), live, format!("changed trigger {name}")),
                        (None, false) => (Some(vec![drop_trigger(d, &name, true)]), live, format!("created trigger {name}")),
                        (None, true) => (None, None, String::new()),
                    };
                    let usable = dropping || present.is_some();
                    let undo = back.filter(|_| usable).map(|s| ddl_undo(conn, &schema, &table, s, vec![Guard { name: name.clone(), present }], label));
                    (Ok(done), undo)
                }
                Err(err) => (Err(err), None),
            };
            Msg::SqlAltered { conn, schema, table, res, undo, sql_mode: None }
        });
    }

    /// How many rows of `table` hold a key that `fk` would find nothing for.
    pub fn sql_orphans(&self, req: u64, db: Db, schema: String, table: String, fk: ForeignKey) {
        self.spawn(async move {
            let Pool::My(pool) = &db.pool;
            let sql = orphans_sql(db.dialect, &schema, &table, &fk);
            let res = sqlx::query(&sql).fetch_one(pool).await.map_err(e).map(|r| super::structure::num(&r, "n").unwrap_or(0));
            Msg::SqlOrphans { req, res }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::lex;
    use crate::db::sql::structure::{Column, Index, IndexCol};

    const M: Dialect = Dialect::MySql;
    const B: Mode = Mode::DEFAULT;
    const NB: Mode = Mode { bs: false, ansi: false };

    fn alt(c: &[Change]) -> String {
        alter(M, "8.4.0", B, "orders", c).unwrap()
    }

    fn def(name: &str, ty: &str) -> ColDef {
        ColDef::new(name, ty)
    }

    fn col(name: &str, ty: &str, nullable: bool, default: Option<&str>) -> Column {
        Column { name: name.into(), full_type: ty.into(), nullable, default: default.map(Into::into), ..Default::default() }
    }

    fn ix(name: &str, unique: bool, cols: &[&str]) -> Index {
        Index { name: name.into(), unique, cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), kind: "BTREE".into(), ..Default::default() }
    }

    fn table() -> Structure {
        let mut id = col("id", "int unsigned", false, None);
        id.extra = "auto_increment".into();
        let mut total = col("total", "decimal(10,2) unsigned", false, Some("0.00"));
        total.comment = "Incl. tax".into();
        let mut note = col("note", "varchar(50)", true, None);
        note.collation = Some("utf8mb4_0900_ai_ci".into());
        Structure { columns: vec![id, col("customer_id", "int unsigned", false, None), total, note], indexes: vec![ix("PRIMARY", true, &["id"]), ix("idx_cust_total", false, &["customer_id", "total"]), ix("uq_note", true, &["note"])], ..Default::default() }
    }

    #[test]
    fn add_a_column() {
        let mut d = def("qty", "INT");
        d.nullable = false;
        d.default = Dv::Value("1".into());
        assert_eq!(alt(&[Change::Add { def: d.clone(), pos: Pos::Keep }]), "ALTER TABLE `orders`\n  ADD COLUMN `qty` INT NOT NULL DEFAULT 1");
        assert!(alt(&[Change::Add { def: d.clone(), pos: Pos::First }]).ends_with("DEFAULT 1 FIRST"));
        assert!(alt(&[Change::Add { def: d, pos: Pos::After("customer_id".into()) }]).ends_with("DEFAULT 1 AFTER `customer_id`"));
    }

    #[test]
    fn modify_the_design_example() {
        let mut d = def("total", "DECIMAL");
        d.args = "12,2".into();
        d.unsigned = true;
        d.nullable = false;
        d.default = Dv::Value("0.00".into());
        d.comment = "Incl. tax".into();
        assert_eq!(alt(&[Change::Modify { from: "total".into(), def: d, pos: Pos::After("customer_id".into()) }]), "ALTER TABLE `orders`\n  MODIFY `total` DECIMAL(12,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' AFTER `customer_id`");
    }

    #[test]
    fn rename_is_change() {
        let c = Change::Modify { from: "note".into(), def: def("memo", "TEXT"), pos: Pos::Keep };
        assert_eq!(alt(&[c]), "ALTER TABLE `orders`\n  CHANGE COLUMN `note` `memo` TEXT NULL DEFAULT NULL");
    }

    #[test]
    fn drop_and_reorder() {
        assert_eq!(alt(&[Change::Drop("note".into())]), "ALTER TABLE `orders`\n  DROP COLUMN `note`");
        let c = Change::Modify { from: "total".into(), def: ColDef::from_column(&table().columns[2]).unwrap(), pos: Pos::First };
        assert_eq!(alt(&[c]), "ALTER TABLE `orders`\n  MODIFY `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' FIRST");
        assert!(alter(M, "8.4.0", B, "orders", &[]).is_err());
    }

    #[test]
    fn indexes() {
        let i = IndexDef { name: "idx_a".into(), kind: IndexKind::Index, cols: vec![IdxCol { name: "a".into(), prefix: Some(10), desc: false }, IdxCol { name: "b".into(), prefix: None, desc: true }], comment: String::new(), ..Default::default() };
        assert_eq!(alt(&[Change::AddIndex(i.clone())]), "ALTER TABLE `orders`\n  ADD INDEX `idx_a` (`a`(10), `b` DESC)");
        let u = IndexDef { kind: IndexKind::Unique, cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], comment: "why".into(), ..i.clone() };
        assert_eq!(alt(&[Change::AddIndex(u)]), "ALTER TABLE `orders`\n  ADD UNIQUE INDEX `idx_a` (`a`) COMMENT 'why'");
        let f = IndexDef { kind: IndexKind::Fulltext, cols: vec![IdxCol { name: "t".into(), prefix: None, desc: false }], ..i.clone() };
        assert_eq!(alt(&[Change::AddIndex(f)]), "ALTER TABLE `orders`\n  ADD FULLTEXT INDEX `idx_a` (`t`)");
        let p = IndexDef { name: "PRIMARY".into(), kind: IndexKind::Primary, cols: vec![IdxCol { name: "id".into(), prefix: None, desc: false }], comment: String::new(), ..Default::default() };
        assert_eq!(alt(&[Change::AddIndex(p)]), "ALTER TABLE `orders`\n  ADD PRIMARY KEY (`id`)");
        assert_eq!(alt(&[Change::DropIndex("idx_a".into())]), "ALTER TABLE `orders`\n  DROP INDEX `idx_a`");
        assert_eq!(alt(&[Change::DropIndex("PRIMARY".into())]), "ALTER TABLE `orders`\n  DROP PRIMARY KEY");
        // A prefix on a FULLTEXT index is refused, and an index needs columns and a name.
        assert!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(IndexDef { kind: IndexKind::Fulltext, ..i.clone() })]).is_err());
        assert!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(IndexDef { cols: vec![], ..i.clone() })]).is_err());
        assert!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(IndexDef { name: String::new(), ..i })]).is_err());
    }

    #[test]
    fn rename_index_by_server_version() {
        let def = IndexDef { name: "old".into(), kind: IndexKind::Unique, cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], comment: String::new(), ..Default::default() };
        let c = [Change::RenameIndex { from: "old".into(), to: "new".into(), def }];
        assert_eq!(alter(M, "8.4.0", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  RENAME INDEX `old` TO `new`");
        assert_eq!(alter(Dialect::MariaDb, "10.11.6-MariaDB", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  RENAME INDEX `old` TO `new`");
        assert_eq!(alter(Dialect::MariaDb, "10.4.0-MariaDB", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  DROP INDEX `old`,\n  ADD UNIQUE INDEX `new` (`a`)");
        assert_eq!(alter(M, "5.6.1", B, "t", &c).unwrap(), "ALTER TABLE `t`\n  DROP INDEX `old`,\n  ADD UNIQUE INDEX `new` (`a`)");
    }

    #[test]
    fn several_changes_make_one_statement() {
        let c = [Change::Drop("note".into()), Change::Add { def: def("qty", "INT"), pos: Pos::After("id".into()) }, Change::DropIndex("uq_note".into())];
        assert_eq!(alt(&c), "ALTER TABLE `orders`\n  DROP COLUMN `note`,\n  ADD COLUMN `qty` INT NULL DEFAULT NULL AFTER `id`,\n  DROP INDEX `uq_note`");
    }

    #[test]
    fn types_defaults_and_flags() {
        let b = |f: &dyn Fn(&mut ColDef)| {
            let mut d = def("x", "VARCHAR");
            f(&mut d);
            definition(M, B, &d)
        };
        assert_eq!(b(&|d| d.args = "255".into()).unwrap(), "VARCHAR(255) NULL DEFAULT NULL");
        assert_eq!(b(&|d| (d.ty, d.args, d.collation) = ("char".into(), "3".into(), Some("utf8mb4_bin".into()))).unwrap(), "CHAR(3) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NULL DEFAULT NULL");
        // A collation on a number is dropped rather than sent.
        assert_eq!(b(&|d| (d.ty, d.collation) = ("INT".into(), Some("utf8mb4_bin".into()))).unwrap(), "INT NULL DEFAULT NULL");
        assert_eq!(b(&|d| (d.ty, d.unsigned, d.zerofill, d.nullable, d.default, d.auto_increment) = ("BIGINT".into(), true, true, false, Dv::None, true)).unwrap(), "BIGINT UNSIGNED ZEROFILL NOT NULL AUTO_INCREMENT");
        assert_eq!(b(&|d| (d.ty, d.args, d.nullable, d.default) = ("ENUM".into(), "'a','it''s'".into(), false, Dv::Value("it's".into()))).unwrap(), "ENUM('a','it''s') NOT NULL DEFAULT 'it''s'");
        assert_eq!(b(&|d| (d.ty, d.nullable, d.default, d.on_update) = ("DATETIME".into(), false, Dv::Expr("CURRENT_TIMESTAMP".into()), Some("CURRENT_TIMESTAMP".into()))).unwrap(), "DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP");
        assert!(b(&|d| d.default = Dv::Expr("UUID()".into())).is_err());
        assert_eq!(b(&|d| (d.args, d.default) = ("36".into(), Dv::Expr("UUID()".into()))).unwrap(), "VARCHAR(36) NULL DEFAULT (UUID())");
        assert_eq!(b(&|d| (d.ty, d.args, d.default) = ("BIT".into(), "3".into(), Dv::Value("101".into()))).unwrap(), "BIT(3) NULL DEFAULT b'101'");
        assert_eq!(b(&|d| (d.ty, d.default) = ("INT".into(), Dv::Value("-1.5e3".into()))).unwrap(), "INT NULL DEFAULT -1.5e3");
        // Text for a number column is quoted, not trusted as a number.
        assert_eq!(b(&|d| (d.ty, d.default) = ("INT".into(), Dv::Value("1 OR 1".into()))).unwrap(), "INT NULL DEFAULT '1 OR 1'");
    }

    #[test]
    fn invalid_definitions_are_refused() {
        let bad = |f: &dyn Fn(&mut ColDef)| {
            let mut d = def("x", "INT");
            f(&mut d);
            definition(M, B, &d).is_err()
        };
        assert!(bad(&|d| d.ty = "INT; DROP TABLE x".into()));
        assert!(bad(&|d| d.ty = String::new()));
        assert!(bad(&|d| d.args = "1) , y INT (".into()));
        assert!(bad(&|d| d.args = "1,2,3".into()));
        assert!(bad(&|d| (d.ty, d.args) = ("ENUM".into(), String::new())));
        assert!(bad(&|d| (d.ty, d.args) = ("ENUM".into(), "'a') x (".into())));
        assert!(bad(&|d| (d.nullable, d.default) = (false, Dv::Null)));
        assert!(bad(&|d| (d.ty, d.auto_increment) = ("VARCHAR".into(), true)));
        assert!(bad(&|d| (d.ty, d.collation) = ("VARCHAR".into(), Some("x COLLATE y; --".into()))));
        assert!(bad(&|d| d.name = String::new()));
        assert!(bad(&|d| d.name = "a".repeat(65)));
        assert!(bad(&|d| d.default = Dv::Expr("1); DROP TABLE x; --".into())));
        assert!(bad(&|d| d.default = Dv::Expr("'unclosed".into())));
        assert!(bad(&|d| d.default = Dv::Expr("1 -- c".into())));
        assert!(bad(&|d| d.default = Dv::Expr("1 # c".into())));
        assert!(bad(&|d| d.default = Dv::Expr("1 /* c */".into())));
        assert!(bad(&|d| d.default = Dv::Expr("1) + (2".into())));
        assert!(!bad(&|d| d.default = Dv::Expr("(1 + 2) * ';'".into())));
    }

    #[test]
    fn hostile_names_and_values_stay_inside_their_quotes() {
        let names = ["a`b", "'; DROP TABLE x; --", "x` INT, DROP COLUMN `y", "back\\slash", "new\nline", "\u{1F600} é"];
        for bs in [true, false] {
            for n in names {
                let mut d = def(n, "VARCHAR");
                d.args = "20".into();
                d.default = Dv::Value(n.into());
                d.comment = n.into();
                let ixd = IndexDef { name: n.into(), kind: IndexKind::Index, cols: vec![IdxCol { name: n.into(), prefix: None, desc: false }], comment: n.into(), ..Default::default() };
                let c = [Change::Add { def: d.clone(), pos: Pos::After(n.into()) }, Change::Modify { from: n.into(), def: ColDef { name: format!("{n}2"), ..d }, pos: Pos::Keep }, Change::Drop(n.into()), Change::AddIndex(ixd), Change::DropIndex(n.into())];
                let sql = alter(M, "8.4.0", Mode { bs, ansi: false }, n, &c).unwrap();
                assert_eq!(lex::split_statements(&sql).len(), 1, "{n} bs={bs}: {sql}");
                // Every backtick that is not a doubled one of an identifier closes an identifier we opened.
                assert!(lex::read_only_check(&sql).is_err());
            }
        }
        let mut d = def("c", "VARCHAR");
        d.args = "20".into();
        d.default = Dv::Value("a\\'; DROP TABLE x; --".into());
        d.comment = "it's \\".into();
        assert_eq!(definition(M, B, &d).unwrap(), "VARCHAR(20) NULL DEFAULT 'a\\\\''; DROP TABLE x; --' COMMENT 'it''s \\\\'");
        assert_eq!(definition(M, NB, &d).unwrap(), "VARCHAR(20) NULL DEFAULT 'a\\''; DROP TABLE x; --' COMMENT 'it''s \\'");
        assert_eq!(M.quote_ident("a`b"), "`a``b`");
        assert_eq!(alter(M, "8", B, "a`b", &[Change::Drop("c`d".into())]).unwrap(), "ALTER TABLE `a``b`\n  DROP COLUMN `c``d`");
    }

    #[test]
    fn values_round_trip() {
        let vals = vec!["a".to_string(), "it's".to_string(), "x,y".to_string(), "back\\slash".to_string(), "new\nline".to_string(), "50\\%".to_string(), String::new()];
        assert_eq!(parse_values(&values_text(&vals)).unwrap(), vals);
        assert!(parse_values("a,b").is_err());
        assert!(parse_values("'a'x").is_err());
        assert!(parse_values("'a").is_err());
        assert_eq!(parse_values("'a' , 'b'").unwrap(), ["a", "b"]);
    }

    /// Review finding 2: COLUMN_TYPE writes `\n` and `\\` in every sql_mode, and an Edit or Move wrote them back as
    /// `n` (and kept `\\` doubled under NO_BACKSLASH_ESCAPES).
    #[test]
    fn enum_values_read_with_escapes_whatever_the_mode() {
        let server = r"'a\nb','c\\d','it''s'";
        assert_eq!(parse_values(server).unwrap(), ["a\nb", "c\\d", "it's"]);
        let mut d = def("e", "ENUM");
        d.args = server.into();
        assert_eq!(definition(M, B, &d).unwrap(), "ENUM('a\nb','c\\\\d','it''s') NULL DEFAULT NULL");
        assert_eq!(definition(M, NB, &d).unwrap(), "ENUM('a\nb','c\\d','it''s') NULL DEFAULT NULL");
        // Read back from what the server shows, a move writes the same values.
        let c = Column { name: "e".into(), full_type: format!("enum({server})"), nullable: true, ..Default::default() };
        let mv = Change::Modify { from: "e".into(), def: ColDef::from_column(&c).unwrap(), pos: Pos::First };
        assert_eq!(alter(M, "8.4.0", NB, "t", &[mv]).unwrap(), "ALTER TABLE `t`\n  MODIFY `e` ENUM('a\nb','c\\d','it''s') NULL DEFAULT NULL FIRST");
    }

    /// Review finding 3: under ANSI_QUOTES `"` opens an identifier where a backslash escapes nothing, so this closed
    /// the expression early and smuggled a second statement.
    #[test]
    fn expressions_follow_ansi_quotes() {
        let x = "\"\\\"); DROP TABLE victim; SELECT (\"";
        let ansi = Mode { bs: true, ansi: true };
        assert!(check_expr(x, ansi).is_err());
        let mut d = def("c", "VARCHAR");
        d.args = "20".into();
        d.default = Dv::Expr(x.into());
        assert!(definition(M, ansi, &d).is_err());
        // Without ANSI_QUOTES it is one string, and the statement stays one statement.
        let sql = alter(M, "8.4.0", B, "t", &[Change::Add { def: d, pos: Pos::Keep }]).unwrap();
        assert_eq!(lex::count_with(&sql, true, false), 1, "{sql}");
        assert!(check_expr("\"a\"\"b\"", ansi).is_ok() && check_expr("'it''s'", ansi).is_ok());
        assert_eq!(Mode::of("ANSI_QUOTES,NO_BACKSLASH_ESCAPES"), Mode { bs: false, ansi: true });
        assert_eq!(Mode::of("REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ONLY_FULL_GROUP_BY,ANSI"), Mode { bs: true, ansi: true });
        assert_eq!(Mode::of("STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION"), B);
    }

    /// Review finding 4: MySQL reports binary defaults as hex and BIT defaults as b'..', which were quoted as text.
    #[test]
    fn binary_and_bit_defaults_keep_their_literal() {
        let b = |d: Dialect, ty: &str, args: &str, v: &str| definition(d, B, &ColDef { args: args.into(), default: Dv::Value(v.into()), ..def("x", ty) }).unwrap();
        assert_eq!(b(M, "VARBINARY", "10", "0x6162"), "VARBINARY(10) NULL DEFAULT 0x6162");
        assert_eq!(b(M, "BINARY", "4", "0x6162"), "BINARY(4) NULL DEFAULT 0x6162");
        assert_eq!(b(M, "VARBINARY", "10", "0x"), "VARBINARY(10) NULL DEFAULT X''");
        assert_eq!(b(M, "BIT", "1", "b'1'"), "BIT(1) NULL DEFAULT b'1'");
        assert_eq!(b(Dialect::MariaDb, "BIT", "3", "b'101'"), "BIT(3) NULL DEFAULT b'101'");
        // MariaDB reports the text itself, so text that looks like hex stays text there, and so does a text column.
        assert_eq!(b(Dialect::MariaDb, "VARBINARY", "10", "0x41"), "VARBINARY(10) NULL DEFAULT '0x41'");
        assert_eq!(b(M, "VARCHAR", "10", "0x41"), "VARCHAR(10) NULL DEFAULT '0x41'");
        assert_eq!(b(M, "VARBINARY", "10", "0xZZ"), "VARBINARY(10) NULL DEFAULT '0xZZ'");
    }

    /// Review finding 5: attributes `from_column` did not read were dropped by an Edit or Move.
    #[test]
    fn column_and_index_attributes_are_kept_or_refused() {
        let p = Column { name: "p".into(), full_type: "point".into(), srid: Some(4326), ..Default::default() };
        assert_eq!(definition(M, B, &ColDef::from_column(&p).unwrap()).unwrap(), "POINT SRID 4326 NOT NULL");
        let j = Column { name: "j".into(), full_type: "longtext".into(), nullable: true, collation: Some("utf8mb4_bin".into()), check: Some("json_valid(`j`)".into()), ..Default::default() };
        let jd = ColDef::from_column(&j).unwrap();
        assert_eq!(definition(Dialect::MariaDb, B, &jd).unwrap(), "JSON NULL DEFAULT NULL");
        let k = Column { name: "k".into(), full_type: "int(11)".into(), check: Some("`k` > 0".into()), ..Default::default() };
        assert!(ColDef::from_column(&k).unwrap_err().contains("CHECK"));
        // A JSON check that names another column is not the JSON alias.
        assert!(ColDef::from_column(&Column { check: Some("json_valid(`x`)".into()), ..j.clone() }).is_err());
        let z = Column { name: "z".into(), full_type: "blob /*M!100301 COMPRESSED*/".into(), ..Default::default() };
        assert!(ColDef::from_column(&z).unwrap_err().contains("COMPRESSED"));
        let mut ix = IndexDef { name: "i".into(), cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], invisible: true, ..Default::default() };
        assert_eq!(alter(M, "8.4.0", B, "t", &[Change::AddIndex(ix.clone())]).unwrap(), "ALTER TABLE `t`\n  ADD INDEX `i` (`a`) INVISIBLE");
        ix.comment = "c".into();
        assert_eq!(alter(Dialect::MariaDb, "11.4.0-MariaDB", B, "t", &[Change::AddIndex(ix)]).unwrap(), "ALTER TABLE `t`\n  ADD INDEX `i` (`a`) COMMENT 'c' IGNORED");
        let read = Index { name: "i".into(), cols: vec![IndexCol { name: "a".into(), ..Default::default() }], kind: "BTREE".into(), invisible: true, ..Default::default() };
        assert!(IndexDef::from_index(&read).invisible);
    }

    #[test]
    fn undo_of_each_shape() {
        let t = table();
        let inv = |c: &[Change]| inverse(M, "8.4.0", B, "orders", &t, c).unwrap().join(";\n");
        assert_eq!(inv(&[Change::Add { def: def("qty", "INT"), pos: Pos::Keep }]), "ALTER TABLE `orders`\n  DROP COLUMN `qty`");
        let mut d = ColDef::from_column(&t.columns[2]).unwrap();
        d.args = "12,2".into();
        assert_eq!(inv(&[Change::Modify { from: "total".into(), def: d.clone(), pos: Pos::Keep }]), "ALTER TABLE `orders`\n  MODIFY `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax'");
        // A move comes back to where the column was, a rename back to the old name.
        d.name = "sum".into();
        assert_eq!(inv(&[Change::Modify { from: "total".into(), def: d, pos: Pos::First }]), "ALTER TABLE `orders`\n  CHANGE COLUMN `sum` `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' AFTER `customer_id`");
        // Dropping a column in a two-column index and a one-column index brings both back.
        assert_eq!(inv(&[Change::Drop("total".into())]), "ALTER TABLE `orders`\n  ADD COLUMN `total` DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00 COMMENT 'Incl. tax' AFTER `customer_id`,\n  DROP INDEX `idx_cust_total`,\n  ADD INDEX `idx_cust_total` (`customer_id`, `total`)");
        assert_eq!(inv(&[Change::Drop("note".into())]), "ALTER TABLE `orders`\n  ADD COLUMN `note` VARCHAR(50) CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci NULL DEFAULT NULL AFTER `total`,\n  ADD UNIQUE INDEX `uq_note` (`note`)");
        let new_ix = IndexDef { name: "i2".into(), kind: IndexKind::Index, cols: vec![IdxCol { name: "id".into(), prefix: None, desc: false }], comment: String::new(), ..Default::default() };
        assert_eq!(inv(&[Change::AddIndex(new_ix)]), "ALTER TABLE `orders`\n  DROP INDEX `i2`");
        assert_eq!(inv(&[Change::DropIndex("uq_note".into())]), "ALTER TABLE `orders`\n  ADD UNIQUE INDEX `uq_note` (`note`)");
        assert_eq!(inv(&[Change::DropIndex("PRIMARY".into())]), "ALTER TABLE `orders`\n  ADD PRIMARY KEY (`id`)");
        let rn = Change::RenameIndex { from: "uq_note".into(), to: "uq_memo".into(), def: IndexDef::from_index(&t.indexes[2]) };
        assert_eq!(inv(&[rn]), "ALTER TABLE `orders`\n  RENAME INDEX `uq_memo` TO `uq_note`");
    }

    /// Review finding 14: a collation that is only the table's default is not pinned on undo.
    #[test]
    fn undo_leaves_the_table_charset_implicit() {
        let mut t = table();
        t.status.collation = "utf8mb4_0900_ai_ci".into();
        assert_eq!(inverse(M, "8.4.0", B, "orders", &t, &[Change::Drop("note".into())]).unwrap()[0], "ALTER TABLE `orders`\n  ADD COLUMN `note` VARCHAR(50) NULL DEFAULT NULL AFTER `total`,\n  ADD UNIQUE INDEX `uq_note` (`note`)");
        t.status.collation = "latin1_swedish_ci".into();
        assert!(inverse(M, "8.4.0", B, "orders", &t, &[Change::Drop("note".into())]).unwrap()[0].contains("CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci"));
        assert!(same_name("Note", "NOTE") && !same_name("note", "notes"));
        assert_eq!(free_name(&["A_copy".into()], "a"), "a_copy2");
    }

    /// Review finding 6: the inverse used to be one ALTER, which the server rejects when a clause names a column
    /// another clause renames or drops. Each step is now its own statement, in order.
    #[test]
    fn undo_of_a_combined_change_runs_backwards() {
        let t = table();
        let c = [Change::Add { def: def("qty", "INT"), pos: Pos::After("id".into()) }, Change::Modify { from: "qty".into(), def: def("amount", "INT"), pos: Pos::Keep }, Change::Drop("customer_id".into())];
        let back = inverse(M, "8.4.0", B, "orders", &t, &c).unwrap();
        assert_eq!(back.len(), 3, "{back:?}");
        assert!(back[0].starts_with("ALTER TABLE `orders`\n  ADD COLUMN `customer_id`") && back[0].contains("AFTER `amount`"), "{back:?}");
        assert!(back[0].contains("ADD INDEX `idx_cust_total` (`customer_id`, `total`)"), "{back:?}");
        assert_eq!(back[1], "ALTER TABLE `orders`\n  CHANGE COLUMN `amount` `qty` INT NULL DEFAULT NULL");
        assert_eq!(back[2], "ALTER TABLE `orders`\n  DROP COLUMN `qty`");
        // No undo for a generated column or an unknown one.
        let mut g = table();
        g.columns[3].extra = "STORED GENERATED".into();
        assert!(inverse(M, "8.4.0", B, "orders", &g, &[Change::Drop("note".into())]).is_err());
        assert!(inverse(M, "8.4.0", B, "orders", &t, &[Change::Drop("nope".into())]).is_err());
    }

    /// The table of the live tests: id, name, note, price, created, kind; idx_name (name), uq_price (price, name).
    fn shop() -> Structure {
        let mut id = col("id", "int unsigned", false, None);
        id.extra = "auto_increment".into();
        let cols = vec![id, col("name", "varchar(50)", false, Some("x")), col("note", "text", true, None), col("price", "decimal(10,2)", false, Some("0.00")), col("created", "datetime", true, None), col("kind", "enum('a','b')", true, None)];
        Structure { columns: cols, indexes: vec![ix("PRIMARY", true, &["id"]), ix("idx_name", false, &["name"]), ix("uq_price", true, &["price", "name"])], ..Default::default() }
    }

    fn renamed(st: &Structure, from: &str, to: &str) -> Change {
        let c = st.columns.iter().find(|c| c.name == from).unwrap();
        Change::Modify { from: from.into(), def: ColDef { name: to.into(), ..ColDef::from_column(c).unwrap() }, pos: Pos::Keep }
    }

    /// The four sequences the review proved fail as one statement, each step naming columns as the steps before it
    /// left them.
    #[test]
    fn undo_steps_name_columns_as_they_are_then() {
        let t = shop();
        let inv = |c: &[Change]| inverse(M, "8.4.0", B, "t", &t, c).unwrap();
        let name = ColDef::from_column(&t.columns[1]).unwrap();
        let back = inv(&[Change::Add { def: def("x", "INT"), pos: Pos::After("id".into()) }, Change::Modify { from: "name".into(), def: name, pos: Pos::After("id".into()) }]);
        assert_eq!(back.len(), 2);
        assert!(back[0].contains("MODIFY `name`") && back[0].ends_with("AFTER `x`") && back[1].ends_with("DROP COLUMN `x`"), "{back:?}");
        let back = inv(&[renamed(&t, "name", "title"), Change::DropIndex("idx_name".into())]);
        assert_eq!(back, ["ALTER TABLE `t`\n  ADD INDEX `idx_name` (`title`)", "ALTER TABLE `t`\n  CHANGE COLUMN `title` `name` VARCHAR(50) NOT NULL DEFAULT 'x'"]);
        let back = inv(&[renamed(&t, "name", "title"), Change::Drop("note".into())]);
        assert!(back[0].contains("ADD COLUMN `note` TEXT NULL DEFAULT NULL AFTER `title`") && back[1].contains("CHANGE COLUMN `title` `name`"), "{back:?}");
        let back = inv(&[renamed(&t, "name", "title"), Change::Drop("price".into())]);
        assert!(back[0].contains("ADD UNIQUE INDEX `uq_price` (`price`, `title`)") && back[1].contains("CHANGE COLUMN `title` `name`"), "{back:?}");
    }

    /// Review finding 8: MariaDB's added-back index named a column by the name the same ALTER renames away.
    #[test]
    fn mariadb_readds_an_index_under_final_names() {
        let t = shop();
        let c = [Change::Drop("price".into()), renamed(&t, "name", "title")];
        let e = expand(Dialect::MariaDb, &t, &c);
        assert!(e.contains(&Change::AddIndex(IndexDef { name: "uq_price".into(), kind: IndexKind::Unique, cols: vec![IdxCol { name: "title".into(), prefix: None, desc: false }], ..Default::default() })), "{e:?}");
        let p = plan(Dialect::MariaDb, "11.4.0-MariaDB", B, "t", &t, &c).unwrap();
        assert!(p.sql.contains("ADD UNIQUE INDEX `uq_price` (`title`)") && !p.sql.contains("(`name`)"), "{}", p.sql);
        assert!(p.undo.is_ok());
    }

    /// Review finding 9: the staged bar showed the statement without MariaDB's index edits. The preview and Apply
    /// both come from `plan`, which expands them.
    #[test]
    fn preview_is_the_statement_that_runs_on_mariadb() {
        let t = table();
        let c = [Change::Drop("note".into())];
        let p = plan(Dialect::MariaDb, "11.4.0-MariaDB", B, "orders", &t, &c).unwrap();
        assert_eq!(p.sql, alter(Dialect::MariaDb, "11.4.0-MariaDB", B, "orders", &expand(Dialect::MariaDb, &t, &c)).unwrap());
        assert_eq!(p.sql, "ALTER TABLE `orders`\n  DROP INDEX `uq_note`,\n  DROP COLUMN `note`");
    }

    /// Review findings 1 and 10: what the undo loses and why there is none are known before Apply.
    #[test]
    fn plan_names_what_undo_loses_or_why_there_is_none() {
        let t = table();
        let p = |c: &[Change]| plan(M, "8.4.0", B, "orders", &t, c).unwrap();
        let add = p(&[Change::Add { def: def("qty", "INT"), pos: Pos::Keep }]).undo.unwrap();
        assert_eq!(add.loss, ["Undo drops column qty and every value it holds."]);
        let mut wide = ColDef::from_column(&t.columns[3]).unwrap();
        wide.args = "255".into();
        let u = p(&[Change::Modify { from: "note".into(), def: wide.clone(), pos: Pos::Keep }]).undo.unwrap();
        assert_eq!(u.loss, ["Undo converts note back to VARCHAR(50), which can cut or reject the values it holds now."]);
        // Renaming and moving touch no values, and undoing a drop only adds.
        wide.args = "50".into();
        wide.name = "memo".into();
        assert!(p(&[Change::Modify { from: "note".into(), def: wide, pos: Pos::First }]).undo.unwrap().loss.is_empty());
        assert!(p(&[Change::Drop("note".into())]).undo.unwrap().loss.is_empty());
        // The undo is checked against these names: the old and new name and the column it goes after.
        let u = p(&[renamed(&t, "note", "memo")]).undo.unwrap();
        assert_eq!((u.cols.clone(), u.idx.clone()), (vec!["memo".to_string(), "note".to_string()], vec![]));
        let u = p(&[Change::Drop("note".into())]).undo.unwrap();
        assert_eq!((u.cols, u.idx), (vec!["note".to_string(), "total".to_string()], vec!["uq_note".to_string()]));
        let mut g = table();
        g.columns[3].extra = "STORED GENERATED".into();
        let none = plan(M, "8.4.0", B, "orders", &g, &[Change::Drop("note".into())]).unwrap();
        assert_eq!(none.sql, "ALTER TABLE `orders`\n  DROP COLUMN `note`");
        assert!(none.undo.unwrap_err().contains("generated"));
    }

    #[test]
    fn snapshots_differ_when_a_touched_column_changed() {
        let t = table();
        let names = (vec!["note".to_string(), "gone".to_string()], vec!["uq_note".to_string()]);
        let a = snapshot(&t, &names.0, &names.1, &[]);
        assert_eq!(a.len(), 3);
        assert!(a[1].1.is_none());
        let mut t2 = t.clone();
        t2.columns[2].comment = "other".into();
        assert_eq!(snapshot(&t2, &names.0, &names.1, &[]), a, "an untouched column does not count");
        t2.columns[3].full_type = "varchar(60)".into();
        assert_ne!(snapshot(&t2, &names.0, &names.1, &[]), a);
        let mut t3 = t.clone();
        t3.indexes[2].invisible = true;
        assert_ne!(snapshot(&t3, &names.0, &names.1, &[]), a);
    }

    /// Review finding 7: a functional key part has no column name, so its drop has no undo.
    #[test]
    fn a_functional_index_has_no_undo() {
        let mut t = table();
        t.indexes.push(Index { name: "f".into(), cols: vec![IndexCol { expr: Some("(`id` + 1)".into()), ..Default::default() }], kind: "BTREE".into(), ..Default::default() });
        let p = plan(M, "8.4.0", B, "orders", &t, &[Change::DropIndex("f".into())]).unwrap();
        assert!(p.undo.unwrap_err().contains("expression"));
        assert!(alter(M, "8.4.0", B, "orders", &[Change::AddIndex(IndexDef::from_index(&t.indexes[3]))]).is_err());
    }

    #[test]
    fn mariadb_drops_the_indexes_a_column_leaves_empty() {
        let t = table();
        let c = [Change::Drop("note".into()), Change::Drop("total".into())];
        assert_eq!(expand(M, &t, &c), c);
        let e = expand(Dialect::MariaDb, &t, &c);
        // uq_note holds only note and goes; idx_cust_total holds total next to customer_id and comes back without it.
        let shrunk = IndexDef { cols: vec![IdxCol { name: "customer_id".into(), prefix: None, desc: false }], ..IndexDef::from_index(&t.indexes[1]) };
        assert_eq!(e, [Change::DropIndex("uq_note".into()), Change::Drop("note".into()), Change::DropIndex("idx_cust_total".into()), Change::AddIndex(shrunk), Change::Drop("total".into())]);
        let back = inverse(Dialect::MariaDb, "11.4.0-MariaDB", B, "orders", &t, &e).unwrap().join(";\n");
        assert!(back.find("ADD COLUMN `total`").unwrap() < back.find("ADD COLUMN `note`").unwrap());
        assert!(back.contains("ADD UNIQUE INDEX `uq_note` (`note`)") && back.contains("ADD INDEX `idx_cust_total` (`customer_id`, `total`)"), "{back}");
    }

    #[test]
    fn order_follows_the_staged_moves() {
        let t = table();
        let c = [Change::Modify { from: "total".into(), def: def("total", "INT"), pos: Pos::First }, Change::Add { def: def("qty", "INT"), pos: Pos::After("id".into()) }, Change::Drop("note".into())];
        assert_eq!(columns_after(&t, &c), ["total", "id", "qty", "customer_id"]);
    }

    #[test]
    fn warnings_name_what_cannot_come_back() {
        let t = table();
        assert_eq!(warnings(&t, &[Change::Drop("note".into())]).len(), 1);
        let mut same = ColDef::from_column(&t.columns[2]).unwrap();
        assert!(warnings(&t, &[Change::Modify { from: "total".into(), def: same.clone(), pos: Pos::Keep }]).is_empty());
        same.ty = "VARCHAR".into();
        assert_eq!(warnings(&t, &[Change::Modify { from: "total".into(), def: same, pos: Pos::Keep }]).len(), 1);
        assert!(drops_data(&[Change::Drop("a".into())]) && !drops_data(&[Change::DropIndex("a".into())]));
        assert_eq!(warnings(&t, &[Change::DropIndex("PRIMARY".into())]).len(), 1);
    }

    #[test]
    fn index_kinds_check_their_columns() {
        let c = |t: &str| vec![("a".to_string(), t.to_string())];
        assert!(index_problem(IndexKind::Fulltext, &c("varchar(20)")).is_none());
        assert!(index_problem(IndexKind::Fulltext, &c("int")).is_some());
        assert!(index_problem(IndexKind::Spatial, &c("point")).is_none());
        assert!(index_problem(IndexKind::Spatial, &c("int")).is_some());
        assert!(index_problem(IndexKind::Unique, &c("int")).is_none());
    }

    #[test]
    fn copy_names_are_free() {
        assert_eq!(free_name(&["a".into()], "a"), "a_copy");
        assert_eq!(free_name(&["a_copy".into(), "a_copy2".into()], "a"), "a_copy3");
    }

    fn fk(name: &str, cols: &[&str], rt: &str, rc: &[&str]) -> ForeignKey {
        ForeignKey { name: name.into(), db: "shop".into(), table: "orders".into(), cols: cols.iter().map(|c| (*c).into()).collect(), ref_db: "shop".into(), ref_table: rt.into(), ref_cols: rc.iter().map(|c| (*c).into()).collect(), on_update: "CASCADE".into(), on_delete: "RESTRICT".into() }
    }

    fn fcol(name: &str, ty: &str, coll: Option<&str>) -> Column {
        Column { name: name.into(), full_type: ty.into(), nullable: false, collation: coll.map(Into::into), ..Default::default() }
    }

    #[test]
    fn add_a_foreign_key() {
        let k = fk("fk_orders_customer", &["customer_id"], "customers", &["id"]);
        assert_eq!(alt(&[Change::AddFk(k.clone())]), "ALTER TABLE `orders`\n  ADD CONSTRAINT `fk_orders_customer`\n    FOREIGN KEY (`customer_id`)\n    REFERENCES `customers` (`id`)\n    ON UPDATE CASCADE ON DELETE RESTRICT");
        // Composite keys keep their column order, and another schema is written out.
        let c = ForeignKey { ref_db: "other".into(), on_delete: "set null".into(), ..fk("fk_c", &["a", "b"], "t", &["x", "y"]) };
        assert_eq!(alt(&[Change::AddFk(c)]), "ALTER TABLE `orders`\n  ADD CONSTRAINT `fk_c`\n    FOREIGN KEY (`a`, `b`)\n    REFERENCES `other`.`t` (`x`, `y`)\n    ON UPDATE CASCADE ON DELETE SET NULL");
        assert_eq!(alt(&[Change::DropFk("fk_orders_customer".into())]), "ALTER TABLE `orders`\n  DROP FOREIGN KEY `fk_orders_customer`");
    }

    #[test]
    fn foreign_key_names_are_quoted_and_checked() {
        // A backtick in any name is doubled, under both escaping modes, since no string literal is involved.
        for bs in [true, false] {
            let k = fk("fk`; DROP TABLE x; --", &["we`ird"], "ta`ble", &["c\"ol"]);
            let sql = alter(M, "8.4.0", Mode { bs, ansi: false }, "or`ders", &[Change::AddFk(k)]).unwrap();
            assert_eq!(sql, "ALTER TABLE `or``ders`\n  ADD CONSTRAINT `fk``; DROP TABLE x; --`\n    FOREIGN KEY (`we``ird`)\n    REFERENCES `ta``ble` (`c\"ol`)\n    ON UPDATE CASCADE ON DELETE RESTRICT", "bs={bs}");
            assert_eq!(alter(M, "8.4.0", Mode { bs, ansi: false }, "t", &[Change::DropFk("a`b".into())]).unwrap(), "ALTER TABLE `t`\n  DROP FOREIGN KEY `a``b`");
        }
        let bad = |k: ForeignKey| alter(M, "8.4.0", B, "t", &[Change::AddFk(k)]);
        assert!(bad(fk("", &["a"], "t", &["b"])).is_err());
        assert!(bad(fk("n", &[], "t", &[])).is_err());
        assert!(bad(fk("n", &["a", "b"], "t", &["x"])).is_err());
        assert!(bad(ForeignKey { on_delete: "CASCADE; DROP".into(), ..fk("n", &["a"], "t", &["b"]) }).is_err());
        assert!(bad(fk("n", &["a"], "", &["b"])).is_err());
    }

    fn fk_table() -> Structure {
        let mut s = table();
        s.foreign_keys = vec![fk("fk_cust", &["customer_id"], "customers", &["id"])];
        s.indexes.push(ix("fk_cust", false, &["customer_id"]));
        s.indexes.retain(|i| i.name != "idx_cust_total");
        s
    }

    #[test]
    fn a_new_key_gets_the_index_it_needs_written_out() {
        let t = table();
        let k = fk("fk_note", &["total"], "notes", &["n"]);
        let e = expand(M, &t, &[Change::AddFk(k.clone())]);
        assert_eq!(e.len(), 2);
        assert!(matches!(&e[0], Change::AddIndex(d) if d.name == "fk_note" && d.cols[0].name == "total"));
        // idx_cust_total starts with customer_id, so a key on it needs nothing more.
        let served = fk("fk_cust", &["customer_id"], "customers", &["id"]);
        assert_eq!(expand(M, &t, &[Change::AddFk(served)]).len(), 1);
        // The index name follows the key's unless that is taken.
        let taken = fk("uq_note", &["note"], "notes", &["n"]);
        assert!(matches!(&expand(M, &t, &[Change::AddFk(ForeignKey { cols: vec!["total".into()], ..taken })])[0], Change::AddIndex(d) if d.name == "uq_note_idx"));
        let sql = alter(M, "8.4.0", B, "orders", &e).unwrap();
        assert!(sql.starts_with("ALTER TABLE `orders`\n  ADD INDEX `fk_note` (`total`),\n  ADD CONSTRAINT `fk_note`"), "{sql}");
    }

    #[test]
    fn dropping_a_key_takes_only_the_index_made_for_it() {
        let t = fk_table();
        let e = expand(M, &t, &[Change::DropFk("FK_CUST".into())]);
        assert_eq!(e, vec![Change::DropFk("FK_CUST".into()), Change::DropIndex("fk_cust".into())]);
        assert_eq!(alter(M, "8.4.0", B, "orders", &e).unwrap(), "ALTER TABLE `orders`\n  DROP FOREIGN KEY `FK_CUST`,\n  DROP INDEX `fk_cust`");
        assert_eq!(warnings(&t, &[Change::DropFk("fk_cust".into())]).len(), 1);
        // An index with another name, or on more columns, was made by someone, so it stays.
        let mut other = t.clone();
        other.indexes.iter_mut().find(|i| i.name == "fk_cust").unwrap().name = "mine".into();
        assert_eq!(expand(M, &other, &[Change::DropFk("fk_cust".into())]).len(), 1);
        assert!(warnings(&other, &[Change::DropFk("fk_cust".into())]).is_empty());
        let mut wide = t.clone();
        wide.indexes.iter_mut().find(|i| i.name == "fk_cust").unwrap().cols.push(IndexCol { name: "total".into(), ..Default::default() });
        assert_eq!(expand(M, &wide, &[Change::DropFk("fk_cust".into())]).len(), 1);
        // Another key that leans on the same index keeps it.
        let mut shared = t.clone();
        shared.foreign_keys.push(fk("fk_other", &["customer_id"], "customers", &["id"]));
        assert_eq!(expand(M, &shared, &[Change::DropFk("fk_cust".into())]).len(), 1);
    }

    #[test]
    fn foreign_key_undo_is_the_exact_inverse() {
        let t = table();
        let k = fk("fk_note", &["total"], "notes", &["n"]);
        let added = expand(M, &t, &[Change::AddFk(k.clone())]);
        assert_eq!(inverse(M, "8.4.0", B, "orders", &t, &added).unwrap(), ["ALTER TABLE `orders`\n  DROP FOREIGN KEY `fk_note`", "ALTER TABLE `orders`\n  DROP INDEX `fk_note`"]);
        let f = fk_table();
        let dropped = expand(M, &f, &[Change::DropFk("fk_cust".into())]);
        assert_eq!(inverse(M, "8.4.0", B, "orders", &f, &dropped).unwrap(), ["ALTER TABLE `orders`\n  ADD INDEX `fk_cust` (`customer_id`)", "ALTER TABLE `orders`\n  ADD CONSTRAINT `fk_cust`\n    FOREIGN KEY (`customer_id`)\n    REFERENCES `customers` (`id`)\n    ON UPDATE CASCADE ON DELETE RESTRICT"]);
        // A key that is not there cannot be dropped, and one that is cannot be added twice.
        assert!(inverse(M, "8.4.0", B, "orders", &t, &[Change::DropFk("nope".into())]).is_err());
        assert!(inverse(M, "8.4.0", B, "orders", &f, &[Change::AddFk(fk("fk_cust", &["total"], "x", &["y"]))]).is_err());
        // The undo is checked against the key and the index made for it, like any other structure undo.
        let u = plan(M, "8.4.0", B, "orders", &t, &[Change::AddFk(k)]).unwrap().undo.unwrap();
        assert_eq!((u.fks, u.idx, u.cols), (vec!["fk_note".to_string()], vec!["fk_note".to_string()], Vec::<String>::new()));
    }

    #[test]
    fn foreign_key_column_types_must_fit() {
        let p = |a: Column, b: Column| fk_pair_problem("orders", &a, "customers", &b);
        let int_u = fcol("id", "int unsigned", None);
        assert!(p(fcol("customer_id", "int unsigned", None), int_u.clone()).is_none());
        assert!(p(fcol("customer_id", "int", None), int_u.clone()).unwrap().contains("signedness"));
        assert!(p(fcol("customer_id", "bigint unsigned", None), int_u.clone()).unwrap().contains("same size"));
        assert!(p(fcol("c", "varchar(10)", None), int_u.clone()).unwrap().contains("same kind"));
        assert!(p(fcol("c", "text", None), fcol("d", "text", None)).unwrap().contains("cannot be part"));
        assert!(p(fcol("c", "json", None), fcol("d", "json", None)).is_some());
        // Strings may differ in length, not in collation.
        assert!(p(fcol("c", "char(3)", Some("utf8mb4_0900_ai_ci")), fcol("code", "varchar(10)", Some("utf8mb4_0900_ai_ci"))).is_none());
        assert!(p(fcol("c", "char(3)", Some("utf8mb4_0900_ai_ci")), fcol("code", "char(3)", Some("latin1_swedish_ci"))).unwrap().contains("collation"));
        assert!(p(fcol("c", "decimal(10,2)", None), fcol("d", "decimal(12,2)", None)).is_some());
        assert!(p(fcol("c", "decimal(10,2)", None), fcol("d", "decimal(10,2)", None)).is_none());
        assert!(p(fcol("c", "datetime", None), fcol("d", "datetime(3)", None)).is_some());
        assert!(p(fcol("c", "datetime(3)", None), fcol("d", "DATETIME(3)", None)).is_none());
    }

    #[test]
    fn a_whole_key_is_checked_before_it_is_staged() {
        let own = Structure { columns: vec![fcol("customer_id", "int unsigned", None), Column { nullable: true, ..fcol("opt", "int unsigned", None) }], ..Default::default() };
        let parent = Structure { columns: vec![fcol("id", "int unsigned", None)], indexes: vec![ix("PRIMARY", true, &["id"])], ..Default::default() };
        let k = fk("fk", &["customer_id"], "customers", &["id"]);
        assert!(fk_problem(&own, "orders", &parent, &k).is_none());
        let set_null = ForeignKey { on_delete: "SET NULL".into(), ..k.clone() };
        assert!(fk_problem(&own, "orders", &parent, &set_null).unwrap().contains("allows NULL"));
        assert!(fk_problem(&own, "orders", &parent, &ForeignKey { cols: vec!["opt".into()], ..set_null }).is_none());
        let unindexed = Structure { indexes: vec![], ..parent.clone() };
        assert!(fk_problem(&own, "orders", &unindexed, &k).unwrap().contains("no index"));
        assert!(fk_problem(&own, "orders", &parent, &ForeignKey { cols: vec!["gone".into()], ..k }).is_some());
    }

    #[test]
    fn orphan_count_statement_quotes_every_name() {
        let k = ForeignKey { ref_db: "o`b".into(), ..fk("n", &["a`1", "b"], "pa`rent", &["x", "y`2"]) };
        assert_eq!(orphans_sql(M, "sh`op", "or`ders", &k), "SELECT COUNT(*) AS n FROM `sh``op`.`or``ders` c WHERE c.`a``1` IS NOT NULL AND c.`b` IS NOT NULL AND NOT EXISTS (SELECT 1 FROM `o``b`.`pa``rent` p WHERE p.`x` = c.`a``1` AND p.`y``2` = c.`b`)");
        let same = ForeignKey { ref_db: String::new(), ..fk("n", &["a"], "p", &["x"]) };
        assert!(orphans_sql(M, "shop", "orders", &same).contains("FROM `shop`.`p` p"));
    }

    fn trig(name: &str, body: &str) -> TrigDef {
        TrigDef { name: name.into(), timing: "before".into(), event: "update".into(), body: body.into() }
    }

    #[test]
    fn trigger_statements_by_engine() {
        let t = trig("orders_bu", "SET NEW.tag = 'x';");
        let one = |d, exists| trigger_steps(d, true, false, "orders", &t, exists).unwrap();
        assert_eq!(one(M, false), ["CREATE TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW\nSET NEW.tag = 'x'"]);
        assert_eq!(one(M, true), ["DROP TRIGGER `orders_bu`", "CREATE TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW\nSET NEW.tag = 'x'"]);
        // MariaDB replaces in place, so an edit is one statement and a new trigger is a plain CREATE.
        assert_eq!(one(Dialect::MariaDb, true), ["CREATE OR REPLACE TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW\nSET NEW.tag = 'x'"]);
        assert!(one(Dialect::MariaDb, false)[0].starts_with("CREATE TRIGGER "));
        assert_eq!(drop_trigger(M, "a`b", false), "DROP TRIGGER `a``b`");
        assert_eq!(drop_trigger(M, "a", true), "DROP TRIGGER IF EXISTS `a`");
        assert_eq!(script(&one(M, true)), "DROP TRIGGER `orders_bu`;\n\nCREATE TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW\nSET NEW.tag = 'x';");
    }

    #[test]
    fn trigger_names_are_quoted_and_the_body_stays_the_users() {
        for bs in [true, false] {
            let t = TrigDef { name: "t`; DROP TABLE x; --".into(), timing: "AFTER".into(), event: "DELETE".into(), body: "BEGIN\n  INSERT INTO log VALUES ('a''b');\nEND".into() };
            let s = trigger_steps(M, bs, false, "or`ders", &t, false).unwrap();
            assert_eq!(s, ["CREATE TRIGGER `t``; DROP TABLE x; --` AFTER DELETE ON `or``ders`\nFOR EACH ROW\nBEGIN\n  INSERT INTO log VALUES ('a''b');\nEND"], "bs={bs}");
        }
        let bad = |t: TrigDef| trigger_steps(M, true, false, "orders", &t, false);
        assert!(bad(trig("", "SET NEW.a = 1")).is_err());
        assert!(bad(TrigDef { timing: "INSTEAD OF".into(), ..trig("a", "SET NEW.a = 1") }).is_err());
        assert!(bad(TrigDef { event: "TRUNCATE".into(), ..trig("a", "SET NEW.a = 1") }).is_err());
        assert!(bad(trig("a", "  ")).is_err());
    }

    #[test]
    fn a_second_statement_in_a_trigger_body_is_refused() {
        assert_eq!(check_body("SET NEW.a = 1;", true, false).unwrap(), "SET NEW.a = 1");
        assert!(check_body("SET NEW.a = 1; DROP TABLE users", true, false).is_err());
        assert!(check_body("-- note\nBEGIN SET NEW.a = 1; SET NEW.b = 2; END;", true, false).is_ok());
        assert!(check_body("begin\nset new.a = 1;\nend", true, false).is_ok());
        // The same text is one statement or two depending on whether a backslash escapes the quote.
        let tricky = r"SET NEW.a = 'x\'; DROP TABLE users; --'";
        assert!(check_body(tricky, true, false).is_ok());
        assert!(check_body(tricky, false, false).is_err());
        // Under ANSI_QUOTES a double quoted ; is an identifier, and either way it is not a statement end.
        assert!(check_body(r#"SET NEW.a = "x;y""#, true, false).is_ok());
        assert!(check_body(r#"SET NEW.a = "x;y""#, true, true).is_ok());
    }

    #[test]
    fn a_foreign_key_snapshot_sees_every_part_of_the_key() {
        let st = fk_table();
        let names = vec!["fk_cust".to_string()];
        let a = snapshot(&st, &[], &[], &names);
        assert!(a[0].1.is_some());
        let mut b = st.clone();
        b.foreign_keys[0].on_delete = "CASCADE".into();
        assert_ne!(snapshot(&b, &[], &[], &names), a);
        b.foreign_keys.clear();
        assert!(snapshot(&b, &[], &[], &names)[0].1.is_none());
    }

    #[test]
    fn trigger_undo_guards_compare_names_without_case() {
        let mut st = fk_table();
        let t = crate::db::sql::structure::Trigger { name: "Tr".into(), timing: "BEFORE".into(), event: "UPDATE".into(), body: " SET a = 1 ".into(), ..Default::default() };
        let tg = Guard { name: "tr".into(), present: Some(crate::db::sql::structure::Trigger { body: "SET a = 1".into(), ..t.clone() }) };
        st.triggers = vec![t.clone()];
        assert!(tg.check(&st).is_ok());
        st.triggers[0].body = "SET a = 2".into();
        assert!(tg.check(&st).is_err());
        st.triggers.clear();
        assert!(Guard { name: "tr".into(), present: None }.check(&st).is_ok());
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use crate::db::sql::structure::Structure;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    fn recv(rx: &Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(30)).expect("no answer from the worker")
    }

    /// Everything about the structure that an ALTER and its undo must leave as it was.
    pub(super) fn shape(st: &Structure) -> Vec<String> {
        let cols = st.columns.iter().map(|c| format!("col {}|{}|{}|{:?}|{}|{}|{:?}|{}|{:?}|{:?}", c.name, c.full_type, c.nullable, c.default, c.default_expr, c.extra, c.collation, c.comment, c.srid, c.check));
        let idx = st.indexes.iter().map(|i| format!("idx {}|{}|{}|{}|{}|{}", i.name, i.unique, i.cols.iter().map(|c| format!("{}:{:?}:{}:{:?}", c.name, c.prefix, c.desc, c.expr)).collect::<Vec<_>>().join(","), i.kind, i.comment, i.invisible));
        cols.chain(idx).collect()
    }

    struct Env {
        w: Worker,
        rx: Receiver<Msg>,
        db: Db,
        pool: sqlx::MySqlPool,
        n: u64,
    }

    impl Env {
        fn exec(&self, sql: &str) {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.unwrap_or_else(|err| panic!("{sql}: {err}")) });
        }

        fn structure(&mut self) -> Structure {
            self.n += 1;
            self.w.sql_structure(self.n, self.db.clone(), "quarry_ddl".into(), "t".into());
            match recv(&self.rx) {
                Msg::SqlStructure { res, .. } => res.unwrap(),
                _ => panic!("expected SqlStructure"),
            }
        }

        fn fresh(&self) {
            self.exec("DROP TABLE IF EXISTS quarry_ddl.t");
            self.exec("CREATE TABLE quarry_ddl.t (
                id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
                name VARCHAR(50) NOT NULL DEFAULT 'x',
                note TEXT NULL,
                price DECIMAL(10,2) NOT NULL DEFAULT 0.00 COMMENT 'p',
                created DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                kind ENUM('a','b') NULL,
                KEY idx_name (name),
                UNIQUE KEY uq_price (price, name)
            ) COMMENT='ddl test'");
            self.exec("INSERT INTO quarry_ddl.t (name, note, price) VALUES ('one', 'n1', 1.50), ('two', NULL, 2.00)");
        }

        /// Applies the changes, checks `after` on the new structure, undoes and checks the old shape is back.
        fn round_trip(&mut self, label: &str, changes: Vec<Change>, after: impl FnOnce(&Structure)) {
            self.fresh();
            let before = self.structure();
            let (d, v) = (self.db.dialect, self.db.version.clone());
            let m = Mode::of(&self.db.sql_mode);
            let p = plan(d, &v, m, "t", &before, &changes).unwrap_or_else(|e| panic!("{label}: {e}"));
            let mut back = p.undo.unwrap_or_else(|e| panic!("{label}: {e}"));
            back.label = label.into();
            let sql = p.sql;
            self.w.sql_alter(1, self.db.clone(), "quarry_ddl".into(), "t".into(), sql.clone(), m, Some(back), false, "done".into());
            let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            res.unwrap_or_else(|e| panic!("{label}: {e}\n{sql}"));
            let changed = self.structure();
            assert_ne!(shape(&before), shape(&changed), "{label}: the change did nothing\n{sql}");
            after(&changed);
            let undo = undo.expect("an undo");
            self.w.sql_undo(self.db.clone(), undo, Vec::new(), false);
            let Msg::SqlAltered { res, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            res.unwrap_or_else(|e| panic!("{label} undo: {e}"));
            assert_eq!(shape(&self.structure()), shape(&before), "{label}: undo did not restore the table\n{sql}");
        }
    }

    fn start(var: &str) -> Option<Env> {
        let uri = std::env::var(var).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let pool = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            for sql in ["DROP DATABASE IF EXISTS quarry_ddl", "CREATE DATABASE quarry_ddl"] {
                sqlx::raw_sql(sql).execute(&p).await.unwrap();
            }
            p
        });
        w.sql_connect(1, uri, Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, ..)), .. } = recv(&rx) else { panic!("expected SqlConnected") };
        Some(Env { w, rx, db, pool, n: 100 })
    }

    fn live(var: &str) {
        let Some(mut e) = start(var) else { return };
        let col = |st: &Structure, n: &str| st.columns.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("no column {n}")).clone();
        let names = |st: &Structure| st.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        let cdef = |e: &mut Env, n: &str| ColDef::from_column(&col(&e.structure(), n)).unwrap();

        e.fresh();
        let mut qty = ColDef::new("qty", "INT");
        qty.nullable = false;
        qty.default = Dv::Value("1".into());
        qty.comment = "how many".into();
        e.round_trip("add column", vec![Change::Add { def: qty.clone(), pos: Pos::After("name".into()) }], |st| {
            assert_eq!(names(st), ["id", "name", "qty", "note", "price", "created", "kind"]);
            let q = col(st, "qty");
            assert!(q.full_type.starts_with("int"));
            assert_eq!((q.nullable, q.default.as_deref(), q.comment.as_str()), (false, Some("1"), "how many"));
        });
        e.round_trip("add column first", vec![Change::Add { def: ColDef { args: "20".into(), ..ColDef::new("first_col", "VARCHAR") }, pos: Pos::First }], |st| assert_eq!(st.columns[0].name, "first_col"));

        e.fresh();
        let mut price = cdef(&mut e, "price");
        price.args = "12,3".into();
        price.comment = "net".into();
        price.default = Dv::Value("0.500".into());
        e.round_trip("modify column", vec![Change::Modify { from: "price".into(), def: price, pos: Pos::Keep }], |st| {
            let p = col(st, "price");
            assert_eq!((p.full_type.as_str(), p.comment.as_str(), p.default.as_deref()), ("decimal(12,3)", "net", Some("0.500")));
        });

        let mut title = cdef(&mut e, "name");
        title.name = "title".into();
        e.round_trip("rename column", vec![Change::Modify { from: "name".into(), def: title, pos: Pos::Keep }], |st| {
            assert!(names(st).contains(&"title".to_string()));
            // The indexes follow the new name.
            assert!(st.indexes.iter().any(|i| i.name == "idx_name" && i.cols[0].name == "title"));
        });

        let kind = cdef(&mut e, "kind");
        e.round_trip("change enum values, default and collation", vec![Change::Modify { from: "kind".into(), def: ColDef { args: values_text(&["a".into(), "b".into(), "it's".into(), "back\\slash".into()]), default: Dv::Value("it's".into()), collation: Some("utf8mb4_bin".into()), ..kind }, pos: Pos::Keep }], |st| {
            let k = col(st, "kind");
            assert_eq!(k.default.as_deref(), Some("it's"));
            assert!(k.full_type.contains("back") && k.full_type.contains("slash") && !k.full_type.contains("backslash"), "{}", k.full_type);
            assert_eq!(k.collation.as_deref(), Some("utf8mb4_bin"));
        });

        let ts = cdef(&mut e, "created");
        e.round_trip("on update", vec![Change::Modify { from: "created".into(), def: ColDef { on_update: Some("CURRENT_TIMESTAMP".into()), ..ts }, pos: Pos::Keep }], |st| assert!(col(st, "created").extra.to_lowercase().contains("on update")));

        e.round_trip("drop column", vec![Change::Drop("note".into())], |st| assert!(!names(st).contains(&"note".to_string())));
        // Dropping a column that sits in a one-column index and in a composite one: undo brings both indexes back.
        e.round_trip("drop indexed column", vec![Change::Drop("name".into())], |st| {
            assert!(!st.indexes.iter().any(|i| i.name == "idx_name"));
            assert_eq!(st.indexes.iter().find(|i| i.name == "uq_price").map(|i| i.cols.len()), Some(1));
        });

        let price_def = cdef(&mut e, "price");
        e.round_trip("reorder first", vec![Change::Modify { from: "price".into(), def: price_def.clone(), pos: Pos::First }], |st| assert_eq!(names(st)[0], "price"));
        e.round_trip("reorder after", vec![Change::Modify { from: "price".into(), def: price_def, pos: Pos::After("kind".into()) }], |st| assert_eq!(names(st).last().unwrap(), "price"));

        let ix = |name: &str, kind: IndexKind, cols: &[(&str, Option<u64>)]| IndexDef { name: name.into(), kind, cols: cols.iter().map(|(n, p)| IdxCol { name: (*n).into(), prefix: *p, desc: false }).collect(), comment: String::new(), ..Default::default() };
        e.round_trip("add index", vec![Change::AddIndex(ix("idx_note", IndexKind::Index, &[("note", Some(10)), ("kind", None)]))], |st| {
            let i = st.indexes.iter().find(|i| i.name == "idx_note").unwrap();
            assert_eq!((i.cols.len(), i.cols[0].prefix), (2, Some(10)));
        });
        e.round_trip("add unique index", vec![Change::AddIndex(ix("uq_name", IndexKind::Unique, &[("name", None)]))], |st| assert!(st.indexes.iter().any(|i| i.name == "uq_name" && i.unique)));
        e.round_trip("add fulltext index", vec![Change::AddIndex(ix("ft_note", IndexKind::Fulltext, &[("note", None)]))], |st| assert!(st.indexes.iter().any(|i| i.name == "ft_note" && i.kind == "FULLTEXT")));
        e.round_trip("drop index", vec![Change::DropIndex("idx_name".into())], |st| assert!(!st.indexes.iter().any(|i| i.name == "idx_name")));
        e.fresh();
        let old = IndexDef::from_index(e.structure().indexes.iter().find(|i| i.name == "uq_price").unwrap());
        e.round_trip("rename index", vec![Change::RenameIndex { from: "uq_price".into(), to: "uq_cost".into(), def: old.clone() }], |st| assert!(st.indexes.iter().any(|i| i.name == "uq_cost" && i.unique && i.cols.len() == 2)));
        // The fallback for servers without RENAME INDEX is these two clauses.
        e.round_trip("rename index by drop and add", vec![Change::DropIndex("uq_price".into()), Change::AddIndex(IndexDef { name: "uq_cost".into(), ..old })], |st| assert!(st.indexes.iter().any(|i| i.name == "uq_cost")));
        let id = ColDef { auto_increment: false, ..cdef(&mut e, "id") };
        e.round_trip("drop primary key", vec![Change::Modify { from: "id".into(), def: id, pos: Pos::Keep }, Change::DropIndex("PRIMARY".into())], |st| assert!(!st.indexes.iter().any(|i| i.name == "PRIMARY")));

        e.fresh();
        let t = e.structure();
        let combined = vec![
            Change::Add { def: ColDef { nullable: false, default: Dv::Value("0".into()), ..ColDef::new("qty", "INT") }, pos: Pos::After("id".into()) },
            Change::Modify { from: "note".into(), def: ColDef { name: "memo".into(), ..ColDef::from_column(&t.columns[2]).unwrap() }, pos: Pos::First },
            Change::Drop("kind".into()),
            Change::AddIndex(ix("idx_qty", IndexKind::Index, &[("qty", None)])),
            Change::DropIndex("idx_name".into()),
        ];
        e.round_trip("combined", combined, |st| {
            assert_eq!(names(st), ["memo", "id", "qty", "name", "price", "created"]);
            assert!(st.indexes.iter().any(|i| i.name == "idx_qty") && !st.indexes.iter().any(|i| i.name == "idx_name"));
        });

        let hostile = "a`b";
        let junk = "'; DROP TABLE t; -- \\ \\' \" `";
        e.round_trip("hostile names and values", vec![Change::Add { def: ColDef { default: Dv::Value(junk.into()), comment: junk.into(), nullable: false, ..ColDef { args: "30".into(), ..ColDef::new(hostile, "VARCHAR") } }, pos: Pos::Keep }, Change::AddIndex(ix(hostile, IndexKind::Index, &[(hostile, Some(5))]))], |st| {
            let c = col(st, hostile);
            assert_eq!(c.default.as_deref(), Some(junk));
            assert_eq!(c.comment, junk);
            assert!(st.indexes.iter().any(|i| i.name == hostile));
        });
        // The table is still there and still holds its rows.
        let rows: i64 = e.w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_ddl.t").fetch_one(&e.pool).await.unwrap() });
        assert_eq!(rows, 2);

        // Writes stay locked until the caller unlocks them.
        e.w.sql_alter(1, e.db.clone(), "quarry_ddl".into(), "t".into(), "ALTER TABLE `t` DROP COLUMN `note`".into(), Mode::of(&e.db.sql_mode), None, true, "done".into());
        let Msg::SqlAltered { res, .. } = recv(&e.rx) else { panic!() };
        assert!(res.is_err());
        assert!(col(&e.structure(), "note").name == "note");

        e.exec("DROP DATABASE quarry_ddl");
    }

    #[test]
    fn live_mysql() {
        live("QUARRY_TEST_MYSQL");
    }

    #[test]
    fn live_mariadb() {
        live("QUARRY_TEST_MARIADB");
    }
}

/// Live tests for the review of e993e30, one per finding the reviewer proved on a server. Database quarry_sf, tables
/// sf_*, so they never meet the tables of `live`.
#[cfg(test)]
mod review_live {
    use super::live::shape;
    use super::*;
    use crate::db::sql::structure::Structure;
    use crate::db::sql::write::SqlUndo;
    use crate::db::sql::Pool;
    use sqlx::Executor;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    const S: &str = "quarry_sf";

    struct R {
        w: Worker,
        rx: Receiver<Msg>,
        db: Db,
        pool: sqlx::MySqlPool,
        n: u64,
    }

    fn recv(rx: &Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(60)).expect("no answer from the worker")
    }

    fn col(st: &Structure, n: &str) -> Column {
        st.columns.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("no column {n}")).clone()
    }

    fn has_col(st: &Structure, n: &str) -> bool {
        st.columns.iter().any(|c| c.name == n)
    }

    fn moved(st: &Structure, n: &str) -> Change {
        Change::Modify { from: n.into(), def: ColDef::in_table(st, &col(st, n)).unwrap(), pos: Pos::First }
    }

    fn renamed(st: &Structure, from: &str, to: &str) -> Change {
        Change::Modify { from: from.into(), def: ColDef { name: to.into(), ..ColDef::in_table(st, &col(st, from)).unwrap() }, pos: Pos::Keep }
    }

    fn alter_undo(a: &UndoAction) -> &AlterUndo {
        match &a.kind {
            UndoKind::Sql { undo: SqlUndo::Alter(u), .. } => u,
            _ => panic!("not a structure undo"),
        }
    }

    impl R {
        fn exec(&self, sql: &str) {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.unwrap_or_else(|err| panic!("{sql}: {err}")) });
        }

        fn rows(&self, sql: &str) -> Vec<String> {
            self.w.rt.block_on(async { sqlx::query_scalar::<_, String>(sql).fetch_all(&self.pool).await.unwrap_or_else(|err| panic!("{sql}: {err}")) })
        }

        fn exists(&self, table: &str) -> bool {
            let q = format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{S}' AND TABLE_NAME = '{table}'");
            self.w.rt.block_on(async { sqlx::query_scalar::<_, i64>(&q).fetch_one(&self.pool).await.unwrap() }) == 1
        }

        fn st(&mut self, db: &Db, table: &str) -> Structure {
            self.n += 1;
            self.w.sql_structure(self.n, db.clone(), S.into(), table.into());
            match recv(&self.rx) {
                Msg::SqlStructure { res, .. } => res.unwrap(),
                _ => panic!("expected SqlStructure"),
            }
        }

        /// The server as it is when its global sql_mode also has `extra`: every session of this pool starts with it.
        /// A pool of its own rather than SET GLOBAL, so the other live tests running at the same time are not affected.
        fn moded(&self, extra: &str) -> Db {
            let mut db = self.db.clone();
            let q = format!("SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',{extra}')");
            let opts = db.opts.clone();
            let pool = self.w.rt.block_on(async move {
                sqlx::mysql::MySqlPoolOptions::new()
                    .max_connections(2)
                    .after_connect(move |c, _| {
                        let q = q.clone();
                        Box::pin(async move { c.execute(q.as_str()).await.map(|_| ()) })
                    })
                    .connect_with(opts)
                    .await
                    .unwrap()
            });
            db.sql_mode = self.w.rt.block_on(async { sqlx::query_scalar::<_, String>("SELECT @@SESSION.sql_mode").fetch_one(&pool).await.unwrap() });
            db.pool = Pool::My(pool);
            db
        }

        fn fresh(&self, table: &str, create: &str) {
            self.exec(&format!("DROP TABLE IF EXISTS {S}.{table}"));
            self.exec(create);
        }

        /// Plans the changes the way Apply does and runs them. Returns the answer, the undo and the statement.
        fn apply(&mut self, db: &Db, table: &str, changes: &[Change]) -> (Res<String>, Option<UndoAction>, Option<String>, String) {
            let st = self.st(db, table);
            let m = Mode::of(&db.sql_mode);
            let p = plan(db.dialect, &db.version, m, table, &st, changes).unwrap_or_else(|e| panic!("{table}: {e}"));
            let undo = p.undo.ok().map(|mut u| {
                u.label = "test".into();
                u
            });
            self.w.sql_alter(1, db.clone(), S.into(), table.into(), p.sql.clone(), m, undo, false, "done".into());
            let Msg::SqlAltered { res, undo, sql_mode, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            (res, undo, sql_mode, p.sql)
        }

        fn undo(&mut self, db: &Db, a: UndoAction) -> (Res<String>, Option<UndoAction>) {
            self.w.sql_undo(db.clone(), a, Vec::new(), false);
            let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            (res, undo)
        }

        /// Creates the table, applies what `changes` builds from its structure, checks `after` with the structure
        /// before and after, undoes, and checks the table is back as it was.
        fn trip(&mut self, db: &Db, table: &str, create: &str, label: &str, changes: impl FnOnce(&Structure) -> Vec<Change>, after: impl FnOnce(&Structure, &Structure)) {
            self.fresh(table, create);
            let before = self.st(db, table);
            let c = changes(&before);
            let (res, undo, _, sql) = self.apply(db, table, &c);
            res.unwrap_or_else(|e| panic!("{label}: {e}\n{sql}"));
            after(&before, &self.st(db, table));
            let (res, _) = self.undo(db, undo.unwrap_or_else(|| panic!("{label}: no undo")));
            res.unwrap_or_else(|e| panic!("{label} undo: {e}\n{sql}"));
            assert_eq!(shape(&self.st(db, table)), shape(&before), "{label}: undo did not restore the table\n{sql}");
        }
    }

    fn start(var: &str) -> Option<R> {
        let uri = std::env::var(var).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let pool = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            for sql in [format!("DROP DATABASE IF EXISTS {S}"), format!("CREATE DATABASE {S}")] {
                sqlx::raw_sql(&sql).execute(&p).await.unwrap();
            }
            p
        });
        w.sql_connect(1, uri, Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, ..)), .. } = recv(&rx) else { panic!("expected SqlConnected") };
        Some(R { w, rx, db, pool, n: 100 })
    }

    fn review(var: &str) {
        let Some(mut r) = start(var) else { return };
        let db = r.db.clone();
        let maria = db.dialect == Dialect::MariaDb;
        finding_1_undo_confirms_loss_and_checks_the_table(&mut r, &db);
        finding_2_enum_values_survive_a_move(&mut r, &db);
        finding_3_and_13_one_statement_in_the_session_mode(&mut r, &db);
        finding_4_binary_and_bit_defaults(&mut r, &db);
        finding_5_attributes(&mut r, &db, maria);
        finding_6_and_8_undo_steps(&mut r, &db);
        if !maria {
            finding_7_functional_index(&mut r, &db);
        }
        r.exec(&format!("DROP DATABASE {S}"));
    }

    fn finding_1_undo_confirms_loss_and_checks_the_table(r: &mut R, db: &Db) {
        let t = "sf_undo";
        r.fresh(t, &format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, note VARCHAR(50) NULL)"));
        r.exec(&format!("INSERT INTO {S}.{t} VALUES (1, 'kept')"));
        let (res, undo, ..) = r.apply(db, t, &[Change::Add { def: ColDef::new("qty", "INT"), pos: Pos::Keep }]);
        res.unwrap();
        let undo = undo.expect("an undo");
        assert_eq!(alter_undo(&undo).loss, ["Undo drops column qty and every value it holds."]);
        // Someone changes the column after the change ran: the undo would not put the table back, so it is refused
        // and nothing runs.
        r.exec(&format!("ALTER TABLE {S}.{t} MODIFY qty BIGINT NULL"));
        let (res, kept) = r.undo(db, undo);
        assert!(res.as_ref().unwrap_err().contains("Undo refused: column qty changed"), "{res:?}");
        assert!(has_col(&r.st(db, t), "qty"));
        // Back as the change left it, the same undo runs.
        r.exec(&format!("ALTER TABLE {S}.{t} MODIFY qty INT NULL DEFAULT NULL"));
        let (res, _) = r.undo(db, kept.expect("the refused undo stays in the list"));
        res.unwrap();
        assert!(!has_col(&r.st(db, t), "qty"));
        // Widening a column: its undo narrows it again, which is named.
        let st = r.st(db, t);
        let wide = Change::Modify { from: "note".into(), def: ColDef { args: "255".into(), ..ColDef::in_table(&st, &col(&st, "note")).unwrap() }, pos: Pos::Keep };
        let (res, undo, ..) = r.apply(db, t, &[wide]);
        res.unwrap();
        assert!(alter_undo(undo.as_ref().unwrap()).loss[0].starts_with("Undo converts note back to VARCHAR(50)"));
        assert_eq!(r.rows(&format!("SELECT note FROM {S}.{t}")), ["kept"]);
    }

    fn finding_2_enum_values_survive_a_move(r: &mut R, db: &Db) {
        let t = "sf_enum";
        for extra in ["", "NO_BACKSLASH_ESCAPES"] {
            let mdb = if extra.is_empty() { db.clone() } else { r.moded(extra) };
            assert_eq!(mdb.backslash_escapes(), extra.is_empty(), "{}", mdb.sql_mode);
            // Created over the default-mode pool, so '\n' is a newline and '\\' one backslash.
            r.fresh(t, &format!(r"CREATE TABLE {S}.{t} (id INT PRIMARY KEY, x INT, e ENUM('a\nb','c\\d') NULL)"));
            r.exec(&format!(r"INSERT INTO {S}.{t} VALUES (1, 0, 'a\nb'), (2, 0, 'c\\d')"));
            let q = format!("SELECT CONCAT(id, ':', HEX(e)) FROM {S}.{t} ORDER BY id");
            assert_eq!(r.rows(&q), ["1:610A62", "2:635C64"]);
            let before = r.st(&mdb, t);
            let (res, undo, _, sql) = r.apply(&mdb, t, &[moved(&before, "e")]);
            res.unwrap_or_else(|e| panic!("mode {extra}: {e}\n{sql}"));
            let after = r.st(&mdb, t);
            assert_eq!(after.columns[0].name, "e");
            assert_eq!(col(&after, "e").full_type, col(&before, "e").full_type, "mode {extra}: {sql}");
            assert_eq!(r.rows(&q), ["1:610A62", "2:635C64"], "mode {extra}: {sql}");
            let (res, _) = r.undo(&mdb, undo.unwrap());
            res.unwrap();
            assert_eq!(shape(&r.st(&mdb, t)), shape(&before));
            assert_eq!(r.rows(&q), ["1:610A62", "2:635C64"]);
        }
    }

    fn finding_3_and_13_one_statement_in_the_session_mode(r: &mut R, db: &Db) {
        let (t, victim) = ("sf_ansi", "sf_victim");
        r.fresh(victim, &format!("CREATE TABLE {S}.{victim} (id INT)"));
        r.fresh(t, &format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, c VARCHAR(20) NULL)"));
        let ansi = r.moded("ANSI_QUOTES");
        let am = Mode::of(&ansi.sql_mode);
        assert!(am.ansi, "{}", ansi.sql_mode);
        let st = r.st(&ansi, t);
        let x = format!("\"\\\"); DROP TABLE {victim}; SELECT (\"");
        let hostile = [Change::Modify { from: "c".into(), def: ColDef { default: Dv::Expr(x), ..ColDef::in_table(&st, &col(&st, "c")).unwrap() }, pos: Pos::Keep }];
        // The check reads quotes as the session does and refuses it.
        assert!(plan(ansi.dialect, &ansi.version, am, t, &st, &hostile).is_err());
        // Past the check (the text a default-mode check lets through), sent to the ANSI_QUOTES session: the
        // prepared statement holds one statement, so the server rejects it and the victim stays.
        let smuggled = alter(ansi.dialect, &ansi.version, Mode::DEFAULT, t, &hostile).unwrap();
        r.w.sql_alter(1, ansi.clone(), S.into(), t.into(), smuggled.clone(), am, None, false, "done".into());
        let Msg::SqlAltered { res, .. } = recv(&r.rx) else { panic!() };
        assert!(res.is_err(), "{smuggled}");
        assert!(r.exists(victim), "{smuggled}");
        // Plain text with a second statement is refused in the default mode too.
        r.w.sql_alter(1, db.clone(), S.into(), t.into(), format!("ALTER TABLE {t} ADD COLUMN z INT; DROP TABLE {victim}"), Mode::of(&db.sql_mode), None, false, "done".into());
        let Msg::SqlAltered { res, .. } = recv(&r.rx) else { panic!() };
        assert!(res.is_err());
        assert!(r.exists(victim) && !has_col(&r.st(db, t), "z"));
        // Finding 13: a statement written for another sql_mode than the session's does not run, and the answer
        // carries the mode in force so the preview can follow it.
        let wrong = Mode { bs: !Mode::of(&db.sql_mode).bs, ansi: false };
        r.w.sql_alter(1, db.clone(), S.into(), t.into(), format!("ALTER TABLE {t} ADD COLUMN z INT"), wrong, None, false, "done".into());
        let Msg::SqlAltered { res, sql_mode, .. } = recv(&r.rx) else { panic!() };
        assert!(res.unwrap_err().contains("sql_mode"));
        assert_eq!(sql_mode.map(|m| Mode::of(&m)), Some(Mode::of(&db.sql_mode)));
        assert!(!has_col(&r.st(db, t), "z"));
        // And through Apply: the ANSI_QUOTES session runs what was planned for it.
        let (res, undo, ..) = r.apply(&ansi, t, &[Change::Add { def: ColDef { default: Dv::Value("say \"hi\"".into()), args: "20".into(), ..ColDef::new("z", "VARCHAR") }, pos: Pos::Keep }]);
        res.unwrap();
        assert_eq!(col(&r.st(&ansi, t), "z").default.as_deref(), Some("say \"hi\""));
        r.undo(&ansi, undo.unwrap()).0.unwrap();
    }

    fn finding_4_binary_and_bit_defaults(r: &mut R, db: &Db) {
        let t = "sf_bin";
        let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, vb VARBINARY(10) DEFAULT 'ab', b4 BINARY(4) DEFAULT 'ab', bt BIT(1) DEFAULT b'1', n INT)");
        for c in ["vb", "b4", "bt"] {
            r.trip(db, t, &create, &format!("move {c}"), |st| vec![moved(st, c)], |before, after| {
                assert_eq!(after.columns[0].name, c);
                let (a, b) = (col(after, c), col(before, c));
                assert_eq!((a.default, a.default_expr, a.full_type), (b.default, b.default_expr, b.full_type), "{c}");
            });
            r.trip(db, t, &create, &format!("drop {c}"), |_| vec![Change::Drop(c.into())], |_, after| assert!(!has_col(after, c)));
        }
        // A row that takes the defaults after a move still gets 'ab' and 1.
        r.fresh(t, &create);
        let st = r.st(db, t);
        let (res, ..) = r.apply(db, t, &[moved(&st, "vb"), moved(&st, "b4"), moved(&st, "bt")]);
        res.unwrap();
        r.exec(&format!("INSERT INTO {S}.{t} (id) VALUES (1)"));
        assert_eq!(r.rows(&format!("SELECT CONCAT(HEX(vb), ',', HEX(b4), ',', bt + 0) FROM {S}.{t}")), ["6162,61620000,1"]);
    }

    fn finding_5_attributes(r: &mut R, db: &Db, maria: bool) {
        let hide = if maria { "IGNORED" } else { "INVISIBLE" };
        let t = "sf_inv";
        let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, a INT, b INT, KEY iv (a) {hide}, KEY ivb (b, a) {hide})");
        r.trip(db, t, &create, "drop invisible index", |_| vec![Change::DropIndex("iv".into())], |before, after| {
            assert!(before.indexes.iter().any(|i| i.name == "iv" && i.invisible));
            assert!(!after.indexes.iter().any(|i| i.name == "iv"));
        });
        r.trip(db, t, &create, "drop column of invisible indexes", |_| vec![Change::Drop("a".into())], |_, after| assert!(after.indexes.iter().all(|i| i.name != "iv")));
        if maria {
            let t = "sf_json";
            let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, j JSON, k INT CHECK (k > 0), z BLOB COMPRESSED, n INT)");
            r.trip(db, t, &create, "move json", |st| vec![moved(st, "j")], |_, after| {
                assert_eq!(after.columns[0].name, "j");
                assert_eq!(col(after, "j").check.as_deref(), Some("json_valid(`j`)"));
            });
            r.trip(db, t, &create, "drop json", |_| vec![Change::Drop("j".into())], |_, after| assert!(!has_col(after, "j")));
            let st = r.st(db, t);
            assert!(ColDef::in_table(&st, &col(&st, "k")).unwrap_err().contains("CHECK"));
            assert!(ColDef::in_table(&st, &col(&st, "z")).unwrap_err().contains("COMPRESSED"));
            // Dropping either has no undo, and the plan says so before anything runs.
            for c in ["k", "z"] {
                let p = plan(db.dialect, &db.version, Mode::of(&db.sql_mode), t, &st, &[Change::Drop(c.into())]).unwrap();
                assert!(p.undo.is_err(), "{c}");
            }
        } else {
            let t = "sf_geo";
            let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, p POINT NOT NULL SRID 4326, n INT)");
            r.trip(db, t, &create, "move srid column", |st| vec![moved(st, "p")], |_, after| {
                assert_eq!(after.columns[0].name, "p");
                assert_eq!(col(after, "p").srid, Some(4326));
            });
            r.trip(db, t, &create, "drop srid column", |_| vec![Change::Drop("p".into())], |_, after| assert!(!has_col(after, "p")));
        }
    }

    fn finding_6_and_8_undo_steps(r: &mut R, db: &Db) {
        let t = "sf_seq";
        let create = format!("CREATE TABLE {S}.{t} (id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY, name VARCHAR(50) NOT NULL DEFAULT 'x', note TEXT NULL, price DECIMAL(10,2) NOT NULL DEFAULT 0.00, KEY idx_name (name), UNIQUE KEY uq_price (price, name))");
        // Moving name up past the new x places it after id, as the Move up button does.
        r.trip(db, t, &create, "add x after id then move name past it", |st| vec![Change::Add { def: ColDef::new("x", "INT"), pos: Pos::After("id".into()) }, Change::Modify { from: "name".into(), def: ColDef::in_table(st, &col(st, "name")).unwrap(), pos: Pos::After("id".into()) }], |_, after| assert!(has_col(after, "x")));
        r.trip(db, t, &create, "rename name to title then drop idx_name", |st| vec![renamed(st, "name", "title"), Change::DropIndex("idx_name".into())], |_, after| assert!(has_col(after, "title")));
        r.trip(db, t, &create, "rename name to title then drop note", |st| vec![renamed(st, "name", "title"), Change::Drop("note".into())], |_, after| assert!(!has_col(after, "note")));
        r.trip(db, t, &create, "rename name to title then drop price", |st| vec![renamed(st, "name", "title"), Change::Drop("price".into())], |_, after| assert!(!has_col(after, "price")));
        // Finding 8: on MariaDB the index added back without price names the column by its new name.
        r.trip(db, t, &create, "drop price then rename name to title", |st| vec![Change::Drop("price".into()), renamed(st, "name", "title")], |_, after| {
            assert!(after.indexes.iter().any(|i| i.name == "uq_price" && i.cols.len() == 1 && i.cols[0].name == "title"), "{:?}", after.indexes);
        });
    }

    fn finding_7_functional_index(r: &mut R, db: &Db) {
        let t = "sf_fn";
        r.fresh(t, &format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, a INT, KEY f ((a + 1)))"));
        let st = r.st(db, t);
        assert!(st.indexes.iter().any(|i| i.name == "f" && i.cols[0].expr.is_some()), "{:?}", st.indexes);
        let p = plan(db.dialect, &db.version, Mode::of(&db.sql_mode), t, &st, &[Change::DropIndex("f".into())]).unwrap();
        assert!(p.undo.unwrap_err().contains("expression"));
    }

    #[test]
    fn review_live_mysql() {
        review("QUARRY_TEST_MYSQL");
    }

    #[test]
    fn review_live_mariadb() {
        review("QUARRY_TEST_MARIADB");
    }
}

/// Foreign keys and triggers against real servers: QUARRY_TEST_MYSQL and QUARRY_TEST_MARIADB, each skipped when
/// unset. Its own schemas (quarry_rt, quarry_rt_b) and `rt_` tables keep it clear of the other live tests; the
/// tests take a lock because they share those schemas.
#[cfg(test)]
mod live_rel {
    use super::*;
    use crate::db::sql::structure::Structure;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn recv(rx: &Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(30)).expect("no answer from the worker")
    }

    /// Keys and indexes as a table holds them, for comparing before and after.
    fn shape(st: &Structure) -> Vec<String> {
        let mut v: Vec<String> = st.indexes.iter().map(|i| format!("idx {}|{}|{}", i.name, i.unique, i.cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(","))).collect();
        v.extend(st.foreign_keys.iter().map(|k| format!("fk {}|{}|{}.{}|{}|{}|{}", k.name, k.cols.join(","), k.ref_db, k.ref_table, k.ref_cols.join(","), k.on_update, k.on_delete)));
        v.sort();
        v
    }

    struct Env {
        w: Worker,
        rx: Receiver<Msg>,
        db: Db,
        pool: sqlx::MySqlPool,
        n: u64,
    }

    impl Env {
        fn try_exec(&self, sql: &str) -> Result<(), String> {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.map(|_| ()).map_err(|e| e.to_string()) })
        }

        fn exec(&self, sql: &str) {
            self.try_exec(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        }

        fn scalar(&self, sql: &str) -> Option<String> {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).fetch_one(&self.pool).await.ok().and_then(|r| super::super::worker::raw(&r, 0).map(|b| String::from_utf8_lossy(b).into_owned())) })
        }

        fn structure_in(&mut self, schema: &str, table: &str) -> Structure {
            self.n += 1;
            self.w.sql_structure(self.n, self.db.clone(), schema.into(), table.into());
            match recv(&self.rx) {
                Msg::SqlStructure { res, .. } => res.unwrap(),
                _ => panic!("expected SqlStructure"),
            }
        }

        fn structure(&mut self, table: &str) -> Structure {
            self.structure_in("quarry_rt", table)
        }

        /// What Apply does for staged changes with foreign keys in them: the same plan and checked ALTER as any other.
        fn apply(&mut self, table: &str, changes: &[Change]) -> Result<UndoAction, String> {
            let before = self.structure(table);
            let m = Mode::of(&self.db.sql_mode);
            let p = plan(self.db.dialect, &self.db.version, m, table, &before, changes)?;
            let mut u = p.undo?;
            u.label = "label".into();
            self.w.sql_alter(1, self.db.clone(), "quarry_rt".into(), table.into(), p.sql.clone(), m, Some(u), false, "done".into());
            let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            res.map_err(|e| format!("{e}\n{}", p.sql))?;
            Ok(undo.expect("an undo"))
        }

        fn undo(&mut self, a: UndoAction) -> Result<(), String> {
            self.w.sql_undo(self.db.clone(), a, Vec::new(), false);
            let Msg::SqlAltered { res, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            res.map(|_| ())
        }

        fn trigger(&mut self, table: &str, def: &TrigDef, exists: bool) -> Result<UndoAction, String> {
            let steps = trigger_steps(self.db.dialect, self.db.backslash_escapes(), false, table, def, exists)?;
            self.w.sql_trigger(1, self.db.clone(), "quarry_rt".into(), table.into(), def.name.clone(), steps, exists, false, false, "done".into());
            let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            res?;
            Ok(undo.expect("an undo"))
        }

        fn drop_trigger(&mut self, table: &str, name: &str) -> Result<UndoAction, String> {
            self.w.sql_trigger(1, self.db.clone(), "quarry_rt".into(), table.into(), name.into(), vec![drop_trigger(self.db.dialect, name, false)], true, true, false, "done".into());
            let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            res?;
            Ok(undo.expect("an undo"))
        }

        /// The tag a fresh row gets from the BEFORE INSERT trigger.
        fn tag(&self) -> Option<String> {
            self.exec("INSERT INTO quarry_rt.rt_trig (tag) VALUES (NULL)");
            self.scalar("SELECT tag FROM quarry_rt.rt_trig ORDER BY id DESC LIMIT 1")
        }
    }

    fn start(var: &str) -> Option<Env> {
        let uri = std::env::var(var).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let pool = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            for sql in [
                "DROP DATABASE IF EXISTS quarry_rt",
                "DROP DATABASE IF EXISTS quarry_rt_b",
                "CREATE DATABASE quarry_rt",
                "CREATE DATABASE quarry_rt_b",
                "CREATE TABLE quarry_rt_b.rt_currencies (code CHAR(3) NOT NULL PRIMARY KEY, label VARCHAR(20))",
                "INSERT INTO quarry_rt_b.rt_currencies VALUES ('EUR', 'Euro')",
                "CREATE TABLE quarry_rt.rt_customers (id INT UNSIGNED NOT NULL PRIMARY KEY, email VARCHAR(40) NOT NULL)",
                "INSERT INTO quarry_rt.rt_customers VALUES (1, 'a@x'), (2, 'b@x')",
                "CREATE TABLE quarry_rt.rt_orders (id INT NOT NULL PRIMARY KEY, customer_id INT UNSIGNED NULL, cur CHAR(3) NULL, note TEXT, wrong INT NULL)",
                "CREATE TABLE quarry_rt.rt_regions (a INT NOT NULL, b INT NOT NULL, PRIMARY KEY (a, b))",
                "INSERT INTO quarry_rt.rt_regions VALUES (1, 2)",
                "CREATE TABLE quarry_rt.rt_lines (id INT NOT NULL PRIMARY KEY, ra INT NULL, rb INT NULL)",
                "CREATE TABLE quarry_rt.rt_trig (id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, tag VARCHAR(20) NULL)",
            ] {
                sqlx::raw_sql(sql).execute(&p).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
            }
            p
        });
        w.sql_connect(1, uri, Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, ..)), .. } = recv(&rx) else { panic!("expected SqlConnected") };
        Some(Env { w, rx, db, pool, n: 100 })
    }

    fn key(name: &str, table: &str, cols: &[&str], ref_db: &str, ref_table: &str, ref_cols: &[&str]) -> ForeignKey {
        ForeignKey { name: name.into(), db: "quarry_rt".into(), table: table.into(), cols: cols.iter().map(|c| (*c).into()).collect(), ref_db: ref_db.into(), ref_table: ref_table.into(), ref_cols: ref_cols.iter().map(|c| (*c).into()).collect(), on_update: "CASCADE".into(), on_delete: "RESTRICT".into() }
    }

    fn foreign_keys(var: &str) {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut e) = start(var) else { return };
        let before = shape(&e.structure("rt_orders"));
        let k = key("fk_rt_cust", "rt_orders", &["customer_id"], "quarry_rt", "rt_customers", &["id"]);
        let (own, parent) = (e.structure("rt_orders"), e.structure("rt_customers"));
        assert_eq!(fk_problem(&own, "rt_orders", &parent, &k), None, "{var}");

        // The orphan count says what would make the ALTER fail, and the ALTER does fail on it.
        e.exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (90, 777), (91, NULL), (92, 1)");
        e.w.sql_orphans(5, e.db.clone(), "quarry_rt".into(), "rt_orders".into(), k.clone());
        let Msg::SqlOrphans { res, .. } = recv(&e.rx) else { panic!("expected SqlOrphans") };
        assert_eq!(res.unwrap(), 1, "{var}: only the row pointing at 777 is an orphan");
        assert!(e.apply("rt_orders", &[Change::AddFk(k.clone())]).is_err(), "{var}: ADD with an orphan row must fail");
        e.exec("DELETE FROM quarry_rt.rt_orders");

        // Add: the index is made, a bad row is refused, a good one is taken.
        let undo = e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap_or_else(|er| panic!("{var}: {er}"));
        let after = e.structure("rt_orders");
        assert_eq!(after.foreign_keys.len(), 1);
        assert!(after.indexes.iter().any(|i| i.name == "fk_rt_cust"), "{var}: {:?}", shape(&after));
        assert!(e.try_exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (1, 4242)").is_err(), "{var}: the key must refuse a missing customer");
        e.exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (2, 1)");
        e.exec("DELETE FROM quarry_rt.rt_orders");
        // Undo is refused when someone changed the key meanwhile.
        e.exec("ALTER TABLE quarry_rt.rt_orders DROP FOREIGN KEY fk_rt_cust");
        let refused = e.undo(undo).unwrap_err();
        assert!(refused.contains("foreign key fk_rt_cust changed"), "{var}: {refused}");
        e.exec("ALTER TABLE quarry_rt.rt_orders DROP INDEX fk_rt_cust");
        assert_eq!(shape(&e.structure("rt_orders")), before, "{var}");

        // Undo of an add restores the table.
        let undo = e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap();
        let with_key = shape(&e.structure("rt_orders"));
        e.undo(undo).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(shape(&e.structure("rt_orders")), before, "{var}: undo of add");

        // Drop takes the index made for the key, and undo brings both back as they were.
        e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap();
        let undo = e.apply("rt_orders", &[Change::DropFk("fk_rt_cust".into())]).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(shape(&e.structure("rt_orders")), before, "{var}: drop took key and index");
        e.undo(undo).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(shape(&e.structure("rt_orders")), with_key, "{var}: undo of drop");
        e.exec("ALTER TABLE quarry_rt.rt_orders DROP FOREIGN KEY fk_rt_cust, DROP INDEX fk_rt_cust");

        // An index somebody made stays when the key goes.
        e.exec("ALTER TABLE quarry_rt.rt_orders ADD INDEX mine (customer_id)");
        let mine = shape(&e.structure("rt_orders"));
        e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap();
        assert!(!e.structure("rt_orders").indexes.iter().any(|i| i.name == "fk_rt_cust"), "{var}: the key had an index to use");
        e.apply("rt_orders", &[Change::DropFk("fk_rt_cust".into())]).unwrap();
        assert_eq!(shape(&e.structure("rt_orders")), mine, "{var}: their index stays");
        e.exec("ALTER TABLE quarry_rt.rt_orders DROP INDEX mine");

        // SET NULL acts as asked.
        let sn = ForeignKey { on_delete: "SET NULL".into(), ..k.clone() };
        let undo = e.apply("rt_orders", &[Change::AddFk(sn)]).unwrap();
        e.exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (3, 2)");
        e.exec("DELETE FROM quarry_rt.rt_customers WHERE id = 2");
        assert_eq!(e.scalar("SELECT customer_id IS NULL FROM quarry_rt.rt_orders WHERE id = 3").as_deref(), Some("1"), "{var}");
        e.exec("INSERT INTO quarry_rt.rt_customers VALUES (2, 'b@x')");
        e.undo(undo).unwrap();
        e.exec("DELETE FROM quarry_rt.rt_orders");
        assert_eq!(shape(&e.structure("rt_orders")), before);

        // A composite key checks both columns together.
        let before_l = shape(&e.structure("rt_lines"));
        let c = key("fk_rt_region", "rt_lines", &["ra", "rb"], "quarry_rt", "rt_regions", &["a", "b"]);
        let undo = e.apply("rt_lines", &[Change::AddFk(c)]).unwrap_or_else(|er| panic!("{var}: {er}"));
        let st = e.structure("rt_lines");
        assert_eq!((st.foreign_keys[0].cols.clone(), st.foreign_keys[0].ref_cols.clone()), (vec!["ra".to_string(), "rb".to_string()], vec!["a".to_string(), "b".to_string()]));
        assert!(e.try_exec("INSERT INTO quarry_rt.rt_lines VALUES (1, 1, 9)").is_err(), "{var}: (1, 9) is not a region");
        e.exec("INSERT INTO quarry_rt.rt_lines VALUES (2, 1, 2)");
        e.exec("DELETE FROM quarry_rt.rt_lines");
        e.undo(undo).unwrap();
        assert_eq!(shape(&e.structure("rt_lines")), before_l, "{var}: composite undo");

        // A key into another schema.
        let x = key("fk_rt_cur", "rt_orders", &["cur"], "quarry_rt_b", "rt_currencies", &["code"]);
        let own = e.structure("rt_orders");
        let parent = e.structure_in("quarry_rt_b", "rt_currencies");
        assert_eq!(fk_problem(&own, "rt_orders", &parent, &x), None, "{var}");
        let undo = e.apply("rt_orders", &[Change::AddFk(x)]).unwrap_or_else(|er| panic!("{var}: {er}"));
        let st = e.structure("rt_orders");
        assert_eq!(st.foreign_keys[0].ref_db, "quarry_rt_b", "{var}");
        assert!(e.try_exec("INSERT INTO quarry_rt.rt_orders (id, cur) VALUES (5, 'XXX')").is_err());
        e.exec("INSERT INTO quarry_rt.rt_orders (id, cur) VALUES (6, 'EUR')");
        e.exec("DELETE FROM quarry_rt.rt_orders");
        e.undo(undo).unwrap();
        assert_eq!(shape(&e.structure("rt_orders")), before, "{var}: cross schema undo");

        // The check agrees with the server about a pair that does not fit.
        let bad = key("fk_rt_bad", "rt_orders", &["wrong"], "quarry_rt", "rt_customers", &["id"]);
        let (own, parent) = (e.structure("rt_orders"), e.structure("rt_customers"));
        assert!(fk_problem(&own, "rt_orders", &parent, &bad).is_some());
        assert!(e.apply("rt_orders", &[Change::AddFk(bad)]).is_err(), "{var}: the server refuses INT against INT UNSIGNED too");

        e.exec("DROP DATABASE quarry_rt");
        e.exec("DROP DATABASE quarry_rt_b");
    }

    fn triggers(var: &str) {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut e) = start(var) else { return };
        let def = |name: &str, body: &str| TrigDef { name: name.into(), timing: "BEFORE".into(), event: "INSERT".into(), body: body.into() };
        let body_of = |e: &mut Env, name: &str| e.structure("rt_trig").triggers.iter().find(|t| t.name == name).map(|t| t.body.clone());

        let a = def("rt_bi", "SET NEW.tag = 'a'");
        let create_undo = e.trigger("rt_trig", &a, false).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(e.tag().as_deref(), Some("a"), "{var}: the trigger fires");

        // Edit: the new body fires.
        let b = def("rt_bi", "SET NEW.tag = 'b'");
        let edit_undo = e.trigger("rt_trig", &b, true).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(e.tag().as_deref(), Some("b"), "{var}");

        // A refused edit leaves the old trigger in place (MySQL dropped it first and has to put it back).
        let bad = def("rt_bi", "SET NEW.nope_column = 1");
        let Err(err) = e.trigger("rt_trig", &bad, true) else { panic!("{var}: a body that names a missing column must be refused") };
        assert!(!err.is_empty());
        assert_eq!(e.tag().as_deref(), Some("b"), "{var}: the old trigger survived {err}");
        assert_eq!(body_of(&mut e, "rt_bi").as_deref(), Some("SET NEW.tag = 'b'"));

        // Undo restores the old body.
        e.undo(edit_undo).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(e.tag().as_deref(), Some("a"), "{var}: undo restored the previous body");
        // Undo is refused when the trigger was changed by someone else.
        let again = e.trigger("rt_trig", &b, true).unwrap();
        e.exec("DROP TRIGGER quarry_rt.rt_bi");
        e.exec("CREATE TRIGGER quarry_rt.rt_bi BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'z'");
        let refused = e.undo(again).unwrap_err();
        assert!(refused.contains("not as this change left it"), "{var}: {refused}");
        assert_eq!(e.tag().as_deref(), Some("z"), "{var}: a refused undo changes nothing");
        e.exec("DROP TRIGGER quarry_rt.rt_bi");
        drop(create_undo);

        // Drop, and undo of a drop.
        e.trigger("rt_trig", &a, false).unwrap();
        let drop_undo = e.drop_trigger("rt_trig", "rt_bi").unwrap_or_else(|er| panic!("{var}: {er}"));
        assert!(body_of(&mut e, "rt_bi").is_none());
        e.undo(drop_undo).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(e.tag().as_deref(), Some("a"), "{var}: undo of drop");
        e.drop_trigger("rt_trig", "rt_bi").unwrap();

        // A compound body with a SIGNAL, a hostile name and a quote in a string.
        let guard = TrigDef { name: "rt`;x".into(), timing: "BEFORE".into(), event: "UPDATE".into(), body: "BEGIN\n  IF NEW.tag = 'it''s bad' THEN\n    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'no';\n  END IF;\nEND".into() };
        let undo = e.trigger("rt_trig", &guard, false).unwrap_or_else(|er| panic!("{var}: {er}"));
        e.exec("INSERT INTO quarry_rt.rt_trig (id, tag) VALUES (500, 'fine')");
        assert!(e.try_exec("UPDATE quarry_rt.rt_trig SET tag = 'it''s bad' WHERE id = 500").is_err(), "{var}: the body ran");
        assert!(e.structure("rt_trig").triggers.iter().any(|t| t.name == "rt`;x"));
        e.undo(undo).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert!(e.structure("rt_trig").triggers.is_empty(), "{var}: undo of create");
        e.exec("UPDATE quarry_rt.rt_trig SET tag = 'it''s bad' WHERE id = 500");

        // Writes stay locked until the caller unlocks them.
        e.w.sql_trigger(1, e.db.clone(), "quarry_rt".into(), "rt_trig".into(), "rt_bi".into(), vec![drop_trigger(e.db.dialect, "rt_bi", true)], false, true, true, "done".into());
        let Msg::SqlAltered { res, .. } = recv(&e.rx) else { panic!() };
        assert!(res.is_err());

        e.exec("DROP DATABASE quarry_rt");
        e.exec("DROP DATABASE quarry_rt_b");
    }

    #[test]
    fn live_foreign_keys_mysql() {
        foreign_keys("QUARRY_TEST_MYSQL");
    }

    #[test]
    fn live_foreign_keys_mariadb() {
        foreign_keys("QUARRY_TEST_MARIADB");
    }

    #[test]
    fn live_triggers_mysql() {
        triggers("QUARRY_TEST_MYSQL");
    }

    #[test]
    fn live_triggers_mariadb() {
        triggers("QUARRY_TEST_MARIADB");
    }
}
