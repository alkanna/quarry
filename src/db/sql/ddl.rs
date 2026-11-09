//! ALTER TABLE for the Structure view: column and index changes are staged as `Change`s, several of
//! them make one statement, and the statements that undo them are worked out from the structure read
//! before applying. Everything here but the worker part is pure text building. Names go through
//! `quote_ident`, strings through `lit`, and the few places that take SQL from the user (an expression
//! default) are checked. Every statement runs as a prepared statement, which holds exactly one.

use super::structure::{load, split_type, Column, Index, Structure};
use super::value::lit;
use super::{Db, Dialect};
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
}

impl Sim {
    fn of(st: &Structure) -> Sim {
        Sim { cols: st.columns.iter().map(|c| (c.name.clone(), ColDef::in_table(st, c))).collect(), idx: st.indexes.iter().map(IndexDef::from_index).collect() }
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

    fn rename_in_indexes(&mut self, from: &str, to: &str) {
        for ix in &mut self.idx {
            for c in ix.cols.iter_mut().filter(|c| same_name(&c.name, from)) {
                c.name = to.into();
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

/// The changes with the index edits the engine will not do by itself: MariaDB refuses to drop a column that an
/// index holds, so each such index is dropped first and added again without the column (or left out when the
/// column was its only one).
pub fn expand(d: Dialect, st: &Structure, changes: &[Change]) -> Vec<Change> {
    if d.drops_index_with_column() {
        return changes.to_vec();
    }
    let mut sim = Sim::of(st);
    let mut out = Vec::new();
    for (k, c) in changes.iter().enumerate() {
        if let Change::Drop(col) = c {
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
        let _ = sim.apply(c);
        out.push(c.clone());
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
    /// The columns and indexes the steps touch.
    pub cols: Vec<String>,
    pub idx: Vec<String>,
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
    let (cols, idx) = touched(&groups);
    Ok(AlterUndo { steps, label: String::new(), loss, mode: m, cols, idx, expect: Vec::new() })
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

/// The column and index names the undo steps read or write, with no repeats.
fn touched(groups: &[Vec<Change>]) -> (Vec<String>, Vec<String>) {
    let (mut cols, mut idx): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
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
        };
        if let Some(Pos::After(n)) = pos {
            add(&mut cols, n);
        }
    }
    (cols, idx)
}

/// How the server shows each named column and index, or None when it is not there. Two snapshots are equal
/// when nothing an undo relies on changed in between.
pub fn snapshot(st: &Structure, cols: &[String], idx: &[String]) -> Vec<(String, Option<String>)> {
    let col = |n: &String| {
        let c = st.columns.iter().find(|c| same_name(&c.name, n));
        (format!("column {n}"), c.map(|c| format!("{}|{}|{}|{:?}|{}|{}|{:?}|{}|{:?}|{:?}", c.name, c.full_type, c.nullable, c.default, c.default_expr, c.extra, c.collation, c.comment, c.srid, c.check)))
    };
    let index = |n: &String| {
        let i = st.indexes.iter().find(|i| same_name(&i.name, n));
        (format!("index {n}"), i.map(|i| format!("{}|{}|{}|{}|{}|{}", i.name, i.unique, i.kind, i.comment, i.invisible, i.cols.iter().map(|c| format!("{}:{:?}:{}:{:?}", c.name, c.prefix, c.desc, c.expr)).collect::<Vec<_>>().join(","))))
    };
    cols.iter().map(col).chain(idx.iter().map(index)).collect()
}

/// What the staged changes cost that undo cannot give back. Shown in the preview.
pub fn warnings(before: &Structure, changes: &[Change]) -> Vec<String> {
    let mut out = Vec::new();
    let sim = Sim::of(before);
    for c in changes {
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

/// A writable connection whose session reads text the way the statements were written for. When the server's
/// sql_mode changed since, nothing runs and the error carries the mode now in force.
async fn session(db: &Db, schema: &str, read_only: bool, m: Mode) -> Result<sqlx::MySqlConnection, (String, Option<String>)> {
    let mut c = super::write::writer(db, schema, read_only).await.map_err(|x| (x, None))?;
    let now: String = sqlx::query_scalar("SELECT @@SESSION.sql_mode").fetch_one(&mut c).await.map_err(|x| (e(x), None))?;
    if Mode::of(&now) != m {
        return Err((format!("Nothing ran: the server's sql_mode is now {now:?}, which reads quotes or backslashes differently from when the statement was written."), Some(now)));
    }
    Ok(c)
}

/// One statement, prepared, so the server parses exactly one whatever the text holds.
async fn exec(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<()> {
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
        let now = snapshot(&st, &u.cols, &u.idx);
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
                        u.expect = snapshot(&st, &u.cols, &u.idx);
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
        let a = snapshot(&t, &names.0, &names.1);
        assert_eq!(a.len(), 3);
        assert!(a[1].1.is_none());
        let mut t2 = t.clone();
        t2.columns[2].comment = "other".into();
        assert_eq!(snapshot(&t2, &names.0, &names.1), a, "an untouched column does not count");
        t2.columns[3].full_type = "varchar(60)".into();
        assert_ne!(snapshot(&t2, &names.0, &names.1), a);
        let mut t3 = t.clone();
        t3.indexes[2].invisible = true;
        assert_ne!(snapshot(&t3, &names.0, &names.1), a);
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
