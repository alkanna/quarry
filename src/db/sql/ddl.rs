//! ALTER TABLE for the Structure view: column and index changes are staged as `Change`s, several of
//! them make one statement, and the statements that undo them are worked out from the structure read
//! before applying. Everything here but the worker part is pure text building. Names go through
//! `quote_ident`, strings through `lit`, and the few places that take SQL from the user (an expression
//! default) are checked. Every statement runs as a prepared statement, which holds exactly one.

use super::se;
use super::structure::{load, split_type, Column, ForeignKey, Index, Structure, Trigger};
use super::{Db, Dialect};
pub use super::ddl_my::alter;
#[cfg(test)]
use super::ddl_my::definition;
#[cfg(test)]
use super::Pool;
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
            return super::ddl_pg::col_def(c);
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
        IndexDef { name: ix.name.clone(), kind, cols: ix.cols.iter().map(|c| IdxCol { name: c.name.clone(), prefix: c.prefix, desc: c.desc }).collect(), comment: ix.comment.clone(), invisible: ix.invisible, functional, constraint: ix.constraint, method, deferrable: ix.deferrable, deferred: ix.deferred, con_comment: ix.con_comment.clone() }
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

/// Whether a PostgreSQL column of this type text can be an identity, for the column dialog.
pub fn pg_identity_type(ty: &str) -> bool {
    super::ddl_pg::identity_type(ty)
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

/// The table as the changes leave it, tracked while their inverses are collected. A column whose definition
/// cannot be read back keeps the reason.
#[derive(Clone)]
pub(super) struct Sim {
    pub(super) cols: Vec<(String, Result<ColDef, String>)>,
    pub(super) idx: Vec<IndexDef>,
    pub(super) fks: Vec<ForeignKey>,
    /// PostgreSQL drops a column together with every index that holds it, puts a new column last and cannot place one.
    pg: bool,
    /// PostgreSQL: what the server drops with each column, as read from the catalog.
    pub(super) deps: Vec<super::structure::ColDep>,
    /// What the inverses collected so far give up, for the undo's confirmation.
    pub(super) notes: Vec<String>,
}

impl Sim {
    pub(super) fn of(st: &Structure) -> Sim {
        Sim { cols: st.columns.iter().map(|c| (c.name.clone(), ColDef::in_table(st, c))).collect(), idx: st.indexes.iter().map(IndexDef::from_index).collect(), fks: st.foreign_keys.clone(), pg: st.pg_schema.is_some(), deps: st.pg_deps.clone(), notes: Vec::new() }
    }

    /// An index the server can use for a key on these columns: it starts with them, in order, whole.
    fn serves(&self, cols: &[String]) -> bool {
        self.idx.iter().any(|ix| matches!(ix.kind, IndexKind::Index | IndexKind::Unique | IndexKind::Primary) && ix.cols.len() >= cols.len() && ix.cols.iter().zip(cols).all(|(a, b)| a.prefix.is_none() && same_name(&a.name, b)))
    }

    /// Where a foreign key is. PostgreSQL keeps `fk` and `FK` as two constraints of one table, so a name is matched exactly
    /// there; MySQL compares them without case.
    pub(super) fn fk_at(&self, name: &str) -> Option<usize> {
        self.fks.iter().position(|k| fk_name_is(self.pg, &k.name, name))
    }

    /// The index to drop along with a key: one named like the key on exactly its columns is what the server
    /// (or this app) made for it. Another key that relies on it keeps it.
    fn backing_index(&self, name: &str) -> Option<String> {
        let fk = &self.fks[self.fk_at(name)?];
        let ix = self.idx.iter().find(|ix| ix.kind == IndexKind::Index && same_name(&ix.name, &fk.name) && ix.cols.len() == fk.cols.len() && ix.cols.iter().zip(&fk.cols).all(|(a, b)| a.prefix.is_none() && !a.desc && same_name(&a.name, b)))?;
        let shared = self.fks.iter().any(|o| !same_name(&o.name, &fk.name) && o.cols.len() <= ix.cols.len() && ix.cols.iter().zip(&o.cols).all(|(a, b)| same_name(&a.name, b)));
        (!shared).then(|| ix.name.clone())
    }

    pub(super) fn at(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|(n, _)| same_name(n, name))
    }

    /// Where a column is now, as the clause that puts it back there.
    fn pos_of(&self, i: usize) -> Pos {
        if i == 0 { Pos::First } else { Pos::After(self.cols[i - 1].0.clone()) }
    }

    fn insert(&mut self, col: (String, Result<ColDef, String>), pos: &Pos) {
        let at = match pos {
            _ if self.pg => self.cols.len(),
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

    /// Applies one change and returns the changes that undo it, in the order to run them. On PostgreSQL a change that is
    /// refused leaves the table as it was, so a caller that goes on after the refusal does not read a half-applied one.
    pub(super) fn apply(&mut self, c: &Change) -> Result<Vec<Change>, String> {
        if !self.pg {
            return self.apply_in_place(c);
        }
        let mut work = self.clone();
        let back = work.apply_in_place(c)?;
        *self = work;
        Ok(back)
    }

    fn apply_in_place(&mut self, c: &Change) -> Result<Vec<Change>, String> {
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
                    for d in self.deps.iter_mut().filter(|d| same_name(&d.col, from)) {
                        d.col = def.name.clone();
                    }
                }
                let back_pos = if *pos == Pos::Keep { Pos::Keep } else { old_pos };
                Ok(vec![Change::Modify { from: def.name.clone(), def: old?, pos: back_pos }])
            }
            Change::Drop(name) if self.pg => {
                let i = self.at(name).ok_or_else(|| format!("Column {name} is not in the table."))?;
                // The server drops the table's own foreign keys that hold the column, and refuses to drop a column that a
                // key of another table points at, so only these have to come back.
                let (fks_gone, fks_keep): (Vec<ForeignKey>, Vec<ForeignKey>) = std::mem::take(&mut self.fks).into_iter().partition(|k| k.cols.iter().any(|c| same_name(c, name)));
                self.fks = fks_keep;
                for k in &fks_gone {
                    if !k.opaque.is_empty() {
                        return Err(format!("Dropping {name} also drops the foreign key {}, which this editor cannot put back: {}.", k.name, k.opaque));
                    }
                    // The column returns empty, and MATCH FULL refuses a key that is partly NULL.
                    if k.match_full && k.cols.len() > 1 {
                        return Err(format!("Dropping {name} also drops the foreign key {}, which is MATCH FULL over several columns: it could not be added back with this column empty.", k.name));
                    }
                }
                let (_, old) = self.cols.remove(i);
                let old = old?;
                // Every index and constraint that holds the column goes whole, so undo adds each one back as it was.
                let (gone, keep): (Vec<IndexDef>, Vec<IndexDef>) = std::mem::take(&mut self.idx).into_iter().partition(|ix| ix.cols.iter().any(|c| same_name(&c.name, name)));
                self.idx = keep;
                // The server also drops what depends on the column without the column being in an index's key parts: a
                // partial index that only names it in its condition, an expression or INCLUDE index, a CHECK, a trigger.
                // Undo restores only the indexes it can write and the column's own default and sequence.
                let lost: Vec<&str> = self.deps.iter().filter(|d| same_name(&d.col, name)).filter(|d| match d.kind.as_str() {
                    "index" => !gone.iter().any(|ix| same_name(&ix.name, &d.name) && !ix.functional),
                    "key" => !gone.iter().any(|ix| same_name(&ix.name, &d.name) && !ix.functional && ix.constraint),
                    "default" => false,
                    "sequence" => !old.auto_increment,
                    "constraint" => !(d.own && fks_gone.iter().any(|k| k.name == d.name)),
                    _ => true,
                }).map(|d| d.what.as_str()).collect();
                if !lost.is_empty() {
                    return Err(format!("Dropping {name} also drops {}, which this editor cannot put back. A foreign key of another table that points at the column makes the server refuse the drop instead.", lost.join(", ")));
                }
                // A NOT NULL column with nothing to fill the rows with cannot come back NOT NULL on a table that has rows,
                // and a primary key needs it, so the column returns nullable and without its primary key.
                let blank = !old.nullable && !old.auto_increment && !matches!(old.default, Dv::Value(_) | Dv::Expr(_));
                let (skipped, gone): (Vec<IndexDef>, Vec<IndexDef>) = gone.into_iter().partition(|ix| blank && ix.kind == IndexKind::Primary);
                let back_def = if blank { ColDef { nullable: true, default: Dv::Null, ..old } } else { old };
                if blank {
                    self.notes.push(format!("Undo adds {name} back empty and allowing NULL{}: it has no default to fill the rows with, so NOT NULL could not be set.", if skipped.is_empty() { "" } else { ", without its primary key" }));
                }
                // The column comes back last: PostgreSQL has no column order to restore it to.
                let mut back = vec![Change::Add { def: back_def, pos: Pos::Keep }];
                // A key of this table on its own key needs that key back first, and it is not when the primary key is skipped.
                for k in fks_gone.iter().filter(|k| same_name(&k.ref_table, &k.table) && same_name(&k.ref_db, &k.db)) {
                    let covered = |ix: &IndexDef| matches!(ix.kind, IndexKind::Unique | IndexKind::Primary) && !ix.functional && ix.cols.len() == k.ref_cols.len() && ix.cols.iter().all(|c| k.ref_cols.iter().any(|r| same_name(r, &c.name)));
                    if !gone.iter().any(covered) && !self.idx.iter().any(covered) {
                        return Err(format!("Dropping {name} also drops the foreign key {}, which points at a key of this table that cannot come back with it.", k.name));
                    }
                }
                back.extend(gone.into_iter().map(Change::AddIndex));
                back.extend(fks_gone.into_iter().map(Change::AddFk));
                Ok(back)
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
                if self.pg {
                    if let Some(last) = self.idx.last_mut() {
                        last.constraint |= last.kind == IndexKind::Primary;
                    }
                    // A primary key makes its columns NOT NULL and dropping it does not undo that, so undo says it too.
                    let mut back = vec![Change::DropIndex(def.name.clone())];
                    if def.kind == IndexKind::Primary {
                        for ic in &def.cols {
                            if let Some(i) = self.at(&ic.name) {
                                if let Ok(old) = &self.cols[i].1.clone() {
                                    if old.nullable {
                                        self.cols[i].1 = Ok(ColDef { nullable: false, ..old.clone() });
                                        back.push(Change::Modify { from: old.name.clone(), def: old.clone(), pos: Pos::Keep });
                                    }
                                }
                            }
                        }
                    }
                    return Ok(back);
                }
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
                if !self.fks[i].opaque.is_empty() {
                    return Err(format!("Foreign key {name} cannot be written back: {}.", self.fks[i].opaque));
                }
                Ok(vec![Change::AddFk(self.fks.remove(i))])
            }
            Change::RenameFk { from, to } => {
                let i = self.fk_at(from).ok_or_else(|| format!("Foreign key {from} is not on the table."))?;
                if !fk_name_is(self.pg, from, to) && self.fk_at(to).is_some() {
                    return Err(format!("The table already has a foreign key called {to}."));
                }
                self.fks[i].name = to.clone();
                Ok(vec![Change::RenameFk { from: to.clone(), to: from.clone() }])
            }
            Change::SetFkDeferral { name, deferrable, deferred } => {
                let i = self.fk_at(name).ok_or_else(|| format!("Foreign key {name} is not on the table."))?;
                let k = &mut self.fks[i];
                let back = Change::SetFkDeferral { name: k.name.clone(), deferrable: k.deferrable, deferred: k.deferred };
                (k.deferrable, k.deferred) = (*deferrable, *deferred);
                Ok(vec![back])
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
            // PostgreSQL does not make an index for a key, and does not need one to hold it.
            Change::AddFk(fk) if d != Dialect::Postgres && !sim.serves(&fk.cols) => {
                let taken = |n: &str| sim.idx.iter().any(|x| same_name(&x.name, n));
                let name = if taken(&fk.name) { format!("{}_idx", fk.name) } else { fk.name.clone() };
                let ix = IndexDef { name, kind: IndexKind::Index, cols: fk.cols.iter().map(|c| IdxCol { name: c.clone(), prefix: None, desc: false }).collect(), ..Default::default() };
                let _ = sim.apply(&Change::AddIndex(ix.clone()));
                out.push(Change::AddIndex(ix));
            }
            _ => {}
        }
        let backing = if let (Change::DropFk(n), true) = (c, d != Dialect::Postgres) { sim.backing_index(n) } else { None };
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
    /// PostgreSQL: the other tables the steps lock and rely on, and the keys they add whose rows are checked first.
    /// `seen` of a table is filled by the worker, from how the table looked right after the change ran.
    pub refs: Vec<RefCheck>,
    pub orphans: Vec<ForeignKey>,
    /// The inverse changes `steps` were written from, one group per step, kept so the steps can be written again
    /// when the session's sql_mode changes before the undo runs.
    pub groups: Vec<Vec<Change>>,
}

impl AlterUndo {
    /// The steps as the session in mode `m` must read them: the stored ones, or the same inverse written again.
    pub fn steps_for(&self, d: Dialect, version: &str, table: &str, m: Mode) -> Result<Vec<String>, String> {
        // PostgreSQL's statements do not depend on the session's quoting.
        if m == self.mode || d == Dialect::Postgres {
            return Ok(self.steps.clone());
        }
        self.groups.iter().map(|g| alter(d, version, m, table, g)).collect()
    }
}

/// The staged changes as they run, and their undo or why there is none. The preview and Apply both use it, so
/// what is shown is what runs.
#[derive(Debug)]
pub struct Plan {
    pub sql: String,
    pub undo: Result<AlterUndo, String>,
    /// The statements that run, one per entry. MySQL has one ALTER; PostgreSQL has one per clause, all in one transaction.
    pub steps: Vec<String>,
    /// PostgreSQL: what the touched columns and indexes looked like when the plan was made. The transaction reads them
    /// again after it holds the lock and refuses to run when they differ. None on MySQL.
    pub expect: Option<Expect>,
}

/// The names a plan reads and `snapshot` of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Expect {
    pub cols: Vec<String>,
    pub idx: Vec<String>,
    pub fks: Vec<String>,
    pub snap: Vec<(String, Option<String>)>,
    /// PostgreSQL: every index, constraint and dependent of the table when the plan was made, for a plan (a column drop)
    /// whose undo relies on the table having none besides these. A name list cannot see one that another session added.
    pub all: Option<Vec<String>>,
    /// PostgreSQL: the type texts the plan carries from the person, which the server must resolve as types.
    pub types: Vec<String>,
    /// PostgreSQL: the other tables the plan locks and relies on (the parents of the keys it adds or drops).
    pub refs: Vec<RefCheck>,
    /// PostgreSQL: the keys it adds, whose existing rows are checked under the lock so the person sees the ones that fail.
    pub orphans: Vec<ForeignKey>,
    /// PostgreSQL: the changes take no more than SHARE ROW EXCLUSIVE on the changed table (they only add keys or change when a
    /// key is checked), where any other change takes ACCESS EXCLUSIVE.
    pub own_share: bool,
}

/// PostgreSQL: a table other than the one being changed that a plan locks. A key that is added relies on the referenced
/// columns and the unique indexes there (`seen` is how they looked, `ddl_pg::ref_state`), and a key that is dropped only
/// needs the lock, which PostgreSQL takes on both tables of a key.
#[derive(Clone, Debug, PartialEq)]
pub struct RefCheck {
    pub schema: String,
    pub table: String,
    pub cols: Vec<String>,
    /// A key is being dropped: ACCESS EXCLUSIVE. Adding one needs SHARE ROW EXCLUSIVE.
    pub exclusive: bool,
    pub seen: Option<Vec<String>>,
}

pub fn plan(d: Dialect, version: &str, m: Mode, table: &str, st: &Structure, staged: &[Change]) -> Result<Plan, String> {
    let changes = expand(d, st, staged);
    if d == Dialect::Postgres {
        return super::ddl_pg::plan(table, st, &changes);
    }
    let sql = alter(d, version, m, table, &changes)?;
    Ok(Plan { steps: vec![sql.clone()], sql, undo: undo_of(d, version, m, table, st, &changes), expect: None })
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
    Ok(AlterUndo { steps, label: String::new(), loss, mode: m, cols, idx, fks, expect: Vec::new(), refs: Vec::new(), orphans: Vec::new(), groups })
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
pub(super) fn undo_loss(after: &Sim, groups: &[Vec<Change>]) -> Vec<String> {
    let mut sim = after.clone();
    let mut out = after.notes.clone();
    for c in groups.iter().flatten() {
        match c {
            Change::Drop(n) => out.push(format!("Undo drops column {n} and every value it holds.")),
            Change::Modify { from, def, .. } => {
                if let Some(Ok(cur)) = sim.at(from).map(|i| &sim.cols[i].1) {
                    if converts(cur, def) {
                        out.push(format!("Undo converts {from} back to {}, which can cut or reject the values it holds now.", type_text(def)));
                    }
                    // Dropping an identity drops its sequence, so the undo starts a new one at the largest value the column holds.
                    if sim.pg && def.auto_increment && !cur.auto_increment {
                        out.push(format!("Undo makes {from} an auto increment again on a new sequence that continues after its largest value: the old sequence's step, limits, cache and position are not restored."));
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
pub(super) fn touched(groups: &[Vec<Change>]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut cols, mut idx, mut fks): (Vec<String>, Vec<String>, Vec<String>) = (Vec::new(), Vec::new(), Vec::new());
    fn add(v: &mut Vec<String>, n: &str) {
        if !v.iter().any(|x| same_name(x, n)) {
            v.push(n.to_string());
        }
    }
    // A key name is kept as typed: PostgreSQL has `fk` and `FK` side by side, and both have to be read back.
    fn add_exact(v: &mut Vec<String>, n: &str) {
        if !v.iter().any(|x| x == n) {
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
                add_exact(&mut fks, &k.name);
                for c in &k.cols {
                    add(&mut cols, c);
                }
                None
            }
            Change::DropFk(n) => {
                add_exact(&mut fks, n);
                None
            }
            Change::RenameFk { from, to } => {
                add_exact(&mut fks, from);
                add_exact(&mut fks, to);
                None
            }
            Change::SetFkDeferral { name, .. } => {
                add_exact(&mut fks, name);
                None
            }
        };
        if let Some(Pos::After(n)) = pos {
            add(&mut cols, n);
        }
    }
    (cols, idx, fks)
}

/// A foreign key in one comparable line, with everything the editor can write back.
pub(super) fn fk_line(k: &ForeignKey) -> String {
    format!("{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}", k.name, k.cols.join(","), k.ref_db, k.ref_table, k.ref_cols.join(","), k.on_update, k.on_delete, k.match_full, k.deferrable, k.deferred, k.not_valid, k.comment)
}

/// PostgreSQL: the referenced columns of a parent table and its unique indexes, in one comparable list. A key that is added
/// leans on both, so the plan keeps how they looked and the transaction compares them again under the lock.
pub fn ref_state(st: &Structure, cols: &[String]) -> Vec<String> {
    let mut v: Vec<String> = cols.iter().map(|n| match st.columns.iter().find(|c| same_name(&c.name, n)) {
        Some(c) => format!("column {}|{}|{:?}", c.name, c.full_type, c.collation),
        None => format!("column {n}|missing"),
    }).collect();
    v.extend(st.indexes.iter().filter(|i| i.unique).map(|i| format!("unique {}|{}|{}|{}", i.name, i.def, i.deferrable, i.deferred)));
    v
}

/// How the server shows each named column, index and foreign key, or None when it is not there. Two snapshots are
/// equal when nothing an undo relies on changed in between. Every part is compared exactly, table names too: both
/// snapshots come from the same server, which spells a name the same way each time whatever lower_case_table_names is.
pub fn snapshot(st: &Structure, cols: &[String], idx: &[String], fks: &[String]) -> Vec<(String, Option<String>)> {
    let col = |n: &String| {
        let c = st.columns.iter().find(|c| same_name(&c.name, n));
        (format!("column {n}"), c.map(|c| format!("{}|{}|{}|{:?}|{}|{}|{:?}|{}|{:?}|{:?}{}", c.name, c.full_type, c.nullable, c.default, c.default_expr, c.extra, c.collation, c.comment, c.srid, c.check, c.default_raw.as_ref().map_or(String::new(), |r| format!("|{r}")))))
    };
    let index = |n: &String| {
        let i = st.indexes.iter().find(|i| same_name(&i.name, n));
        (format!("index {n}"), i.map(|i| format!("{}|{}|{}|{}|{}|{}{}", i.name, i.unique, i.kind, i.comment, i.invisible, i.cols.iter().map(|c| format!("{}:{:?}:{}:{:?}", c.name, c.prefix, c.desc, c.expr)).collect::<Vec<_>>().join(","), if i.def.is_empty() { String::new() } else { format!("|{}|{}|{}|{}|{}|{}|{}", i.def, i.constraint, i.primary, i.complex, i.deferrable, i.deferred, i.con_comment) })))
    };
    let fk = |n: &String| {
        let k = st.foreign_keys.iter().find(|k| fk_name_is(st.pg_schema.is_some(), &k.name, n));
        (format!("foreign key {n}"), k.map(fk_line))
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
            if let Some(ix) = fk_sim.backing_index(n).filter(|_| before.pg_schema.is_none()) {
                out.push(format!("Dropping {n} also drops the index {ix}, which was made for it. Undo brings both back."));
            }
        }
        if let (Change::AddFk(k), Some(_)) = (c, &before.pg_schema) {
            if !fk_sim.serves(&k.cols) {
                out.push(format!("PostgreSQL does not index the columns of a foreign key. Without an index on ({}), deleting or changing a row of {} scans this table.", k.cols.join(", "), k.ref_table));
            }
            if k.not_valid {
                out.push(format!("{} is added NOT VALID: the rows already in the table are not checked, and undo brings back the same unchecked key.", k.name));
            }
        }
        let _ = fk_sim.apply(c);
        match c {
            Change::Drop(n) if before.pg_schema.is_some() => {
                let blank = sim.at(n).and_then(|i| sim.cols[i].1.as_ref().ok()).is_some_and(|d| !d.nullable && !d.auto_increment && !matches!(d.default, Dv::Value(_) | Dv::Expr(_)));
                let back = if blank { "allowing NULL and without its primary key, with its other indexes." } else { "with its indexes." };
                out.push(format!("Dropping {n} deletes its values for good, with every index and constraint that uses it. Undo brings the column back empty and last, {back}"));
            }
            Change::Add { def, .. } if before.pg_schema.is_some() && !def.auto_increment && before.status.rows.is_some_and(|r| r > 0) => {
                if let Dv::Expr(x) = &def.default {
                    if super::ddl_pg::calls_function(x) {
                        out.push(format!("The default of {} calls a function. Any function call is read as volatile here, since the editor does not look the function up. A volatile one runs once for each of the table's rows while PostgreSQL holds an exclusive lock on the table, and again on every later insert.", def.name));
                    }
                }
            }
            Change::Drop(n) => out.push(format!("Dropping {n} deletes its values for good. Undo brings the column back empty.")),
            Change::Modify { from, def, .. } => {
                if let Some(Ok(old)) = sim.at(from).map(|i| &sim.cols[i].1) {
                    if converts(old, def) {
                        out.push(format!("Changing {from} converts its values and can cut or reject some. Undo restores the definition, not the values."));
                    }
                }
            }
            Change::DropIndex(n) if n == "PRIMARY" || before.indexes.iter().any(|i| i.primary && i.name == *n) => out.push("Dropping the primary key makes the rows read-only here until the table has a key again.".into()),
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

/// The comparison family of a PostgreSQL column type, for the types the server compares across names: the integers, the
/// numeric types, the floats, text and varchar, and the date and time types of one family. A type outside the list (a domain,
/// an enum, an extension type) has none, so a key on it is left to the server.
fn pg_family(full_type: &str) -> Option<&'static str> {
    let base = full_type.split('(').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match base.as_str() {
        "smallint" | "integer" | "bigint" => "int",
        "numeric" | "decimal" => "numeric",
        "real" | "double precision" => "float",
        // character(n) is compared with text through the implicit cast, so a key between the two is accepted.
        "text" | "character varying" | "character" => "text",
        "date" | "timestamp without time zone" | "timestamp with time zone" => "datetime",
        "uuid" => "uuid",
        "boolean" => "boolean",
        "bytea" => "bytea",
        "time without time zone" => "time",
        "interval" => "interval",
        _ => return None,
    })
}

/// `fk_pair_problem` for PostgreSQL: a key needs types that the server can compare with `=`, which is any two integer
/// sizes, an integer child against a numeric parent, any text type against another, and the same type otherwise. A numeric
/// child against an integer parent is refused by the server ("cannot be implemented"), so it is refused here. Types it
/// cannot place are left to the server's own answer.
pub fn fk_pair_problem_pg(child_table: &str, child: &Column, parent_table: &str, parent: &Column) -> Option<String> {
    let (fc, fp) = (pg_family(&child.full_type)?, pg_family(&parent.full_type)?);
    if fc == fp || (fc, fp) == ("int", "numeric") {
        return None;
    }
    Some(format!("{child_table}.{} is {} but {parent_table}.{} is {}. A foreign key needs two types that PostgreSQL can compare, such as two integers or two text types.", child.name, child.full_type, parent.name, parent.full_type))
}

/// The first problem with a whole key, before the server is asked: column pairs that do not fit, SET NULL on a
/// column that cannot be NULL, and a referenced side with no index to lean on. `whole_key` is MySQL 8.4's
/// restrict_fk_on_non_standard_key: the referenced columns must then be exactly the primary key or a unique key,
/// where older servers take any index that starts with them.
pub fn fk_problem(own: &Structure, table: &str, parent: &Structure, fk: &ForeignKey, whole_key: bool) -> Option<String> {
    let find = |st: &Structure, n: &str| st.columns.iter().find(|c| same_name(&c.name, n)).cloned();
    let pg = own.pg_schema.is_some();
    for (a, b) in fk.cols.iter().zip(&fk.ref_cols) {
        let (Some(ca), Some(cb)) = (find(own, a), find(parent, b)) else { return Some("A picked column is not in its table any more.".into()) };
        if let Some(p) = if pg { fk_pair_problem_pg(table, &ca, &fk.ref_table, &cb) } else { fk_pair_problem(table, &ca, &fk.ref_table, &cb) } {
            return Some(p);
        }
        let set_null = fk.on_delete.eq_ignore_ascii_case("SET NULL") || fk.on_update.eq_ignore_ascii_case("SET NULL");
        if set_null && !ca.nullable {
            return Some(format!("SET NULL needs a column that allows NULL, and {table}.{a} does not."));
        }
    }
    if pg {
        let twice = |v: &[String]| v.iter().enumerate().find(|(i, c)| v[..*i].iter().any(|o| same_name(o, c))).map(|(_, c)| c.clone());
        if let Some(c) = twice(&fk.cols).or_else(|| twice(&fk.ref_cols)) {
            return Some(format!("The column {c} is picked twice in one key."));
        }
        // PostgreSQL wants a primary key or unique constraint (or a unique index) on exactly the referenced columns, in any
        // order. A condition on the index is not visible here, so the server has the last word on that.
        let exact = parent.indexes.iter().any(|ix| ix.unique && !ix.deferrable && ix.cols.len() == fk.ref_cols.len() && ix.cols.iter().all(|c| c.expr.is_none() && fk.ref_cols.iter().any(|r| same_name(r, &c.name))));
        return (!exact).then(|| format!("PostgreSQL needs a primary key or unique constraint on exactly ({}) of {}, and there is none. Add one there first.", fk.ref_cols.join(", "), fk.ref_table));
    }
    let starts = |ix: &Index| ix.cols.len() >= fk.ref_cols.len() && ix.cols.iter().zip(&fk.ref_cols).all(|(x, y)| x.prefix.is_none() && x.expr.is_none() && same_name(&x.name, y));
    if whole_key {
        let exact = parent.indexes.iter().any(|ix| ix.unique && ix.cols.len() == fk.ref_cols.len() && starts(ix));
        return (!exact).then(|| format!("This server only lets a key point at a whole primary or unique key, and {} has none on exactly ({}). Add a unique index there first.", fk.ref_table, fk.ref_cols.join(", ")));
    }
    let led = parent.indexes.iter().any(starts);
    (!led).then(|| format!("{} has no index that starts with ({}). Add an index there first.", fk.ref_table, fk.ref_cols.join(", ")))
}

/// Whether a server is taken to refuse keys on part of a key until it says: MySQL 8.4 and later turn
/// restrict_fk_on_non_standard_key on by default.
pub fn fk_whole_key_default(d: Dialect, version: &str) -> bool {
    let mut n = version.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse::<u32>().ok());
    matches!(d, Dialect::MySql) && (n.next().unwrap_or(0), n.next().unwrap_or(0)) >= (8, 4)
}

/// The condition that makes a row of the child fail `fk`, written for the alias `c` of the child. MATCH SIMPLE skips a row with
/// any NULL key column and needs a parent for the others. MATCH FULL needs a parent for a row with none, and also refuses
/// one with some key columns NULL and some not.
fn orphan_cond(d: Dialect, parent: &str, fk: &ForeignKey) -> String {
    let q = |n: &str| d.quote_ident(n);
    let own: Vec<String> = fk.cols.iter().map(|c| format!("c.{}", q(c))).collect();
    let present = own.iter().map(|c| format!("{c} IS NOT NULL")).collect::<Vec<_>>().join(" AND ");
    let same = own.iter().zip(&fk.ref_cols).map(|(a, b)| format!("p.{} = {a}", q(b))).collect::<Vec<_>>().join(" AND ");
    let missing = format!("{present} AND NOT EXISTS (SELECT 1 FROM {parent} p WHERE {same})");
    if !fk.match_full || fk.cols.len() < 2 {
        return missing;
    }
    let some_null = own.iter().map(|c| format!("{c} IS NULL")).collect::<Vec<_>>().join(" OR ");
    let some_set = own.iter().map(|c| format!("{c} IS NOT NULL")).collect::<Vec<_>>().join(" OR ");
    format!("(({missing}) OR (({some_null}) AND ({some_set})))")
}

/// Counts the rows of the table that `fk` would refuse, which is what makes ADD FOREIGN KEY fail.
pub fn orphans_sql(d: Dialect, schema: &str, table: &str, fk: &ForeignKey) -> String {
    let parent = d.qualified(if fk.ref_db.is_empty() { schema } else { &fk.ref_db }, &fk.ref_table);
    format!("SELECT COUNT(*) AS n FROM {} c WHERE {}", d.qualified(schema, table), orphan_cond(d, &parent, fk))
}

/// A few of the rows `orphans_sql` counts, each as its key values written as text, for the message that shows the person
/// which rows stop the key from being added. PostgreSQL only. It is a subselect so the LIMIT the metadata reads append
/// still fits.
pub fn orphan_sample_sql(d: Dialect, schema: &str, table: &str, fk: &ForeignKey, limit: u32) -> String {
    let q = |n: &str| d.quote_ident(n);
    let parent = d.qualified(if fk.ref_db.is_empty() { schema } else { &fk.ref_db }, &fk.ref_table);
    let shown = fk.cols.iter().enumerate().map(|(i, c)| format!("c.{}::text AS k{i}", q(c))).collect::<Vec<_>>().join(", ");
    format!("SELECT * FROM (SELECT {shown} FROM {} c WHERE {} LIMIT {limit}) s", d.qualified(schema, table), orphan_cond(d, &parent, fk))
}

/// What the orphan count depends on: the name and the actions of a key do not change which rows point at nothing,
/// so typing a name does not count again.
pub fn orphan_key(fk: &ForeignKey) -> (Vec<String>, String, String, Vec<String>) {
    (fk.cols.clone(), fk.ref_db.clone(), fk.ref_table.clone(), fk.ref_cols.clone())
}

/// A trigger as the editor holds it. The body is the user's own SQL, written after FOR EACH ROW as typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrigDef {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub body: String,
}

/// The blocks a trigger body nests. A CASE inside an expression ends at a bare END, a CASE statement at END CASE.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Blk {
    Begin,
    If,
    CaseStmt,
    CaseExpr,
    Loop,
    While,
    Repeat,
    For,
}

impl Blk {
    /// The word after END that closes this block, or None for a bare END.
    fn end_word(self) -> Option<&'static str> {
        match self {
            Blk::Begin | Blk::CaseExpr => None,
            Blk::If => Some("IF"),
            Blk::CaseStmt => Some("CASE"),
            Blk::Loop => Some("LOOP"),
            Blk::While => Some("WHILE"),
            Blk::Repeat => Some("REPEAT"),
            Blk::For => Some("FOR"),
        }
    }
}

/// The body without its trailing `;`, refused unless the server reads all of it as the one statement after FOR EACH
/// ROW. The text goes over the text protocol, where CREATE TRIGGER has to (it cannot be prepared) and where a second
/// statement would run too, so the check follows the server's grammar: a body is one plain statement, or one
/// compound statement (BEGIN, IF, CASE, LOOP, WHILE, REPEAT or MariaDB's FOR, maybe labelled) whose blocks are
/// tracked to the END that closes it. Only a `;` may follow that END. When the check cannot tell what the server
/// would read it refuses rather than guesses: it may see a block close earlier than the server (and refuse what
/// follows), never later, so a block opens here only where the grammar opens one and every END closes one.
/// Strings and comments are read with the session's quote rules, and executable comments are refused because the
/// server runs or skips them by version.
pub fn check_body(body: &str, m: Mode) -> Result<String, String> {
    use super::lex::{tokens_with, Tok};
    let toks: Vec<(Tok, std::ops::Range<usize>)> = tokens_with(body, super::Dialect::MySql, m.bs, m.ansi).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    if toks.iter().any(|(t, _)| matches!(t, Tok::ExecOpen | Tok::ExecClose)) {
        return Err("A trigger body cannot hold an executable comment (/*! ... */): whether the server runs it depends on its version.".into());
    }
    let Some(first) = toks.first() else { return Err("The trigger body is empty.".into()) };
    let text = |k: usize| &body[toks[k].1.clone()];
    let word = |k: usize, w: &str| k < toks.len() && toks[k].0 == Tok::Word && text(k).eq_ignore_ascii_case(w);
    let punct = |k: usize, p: &str| k < toks.len() && toks[k].0 == Tok::Punct && text(k) == p;
    // A reserved word after a dot is a name (NEW.end, NEW.case), never a keyword.
    let after_dot = |k: usize| k > 0 && punct(k - 1, ".");
    let unquote = |s: &str| if s.len() >= 2 && (s.starts_with('`') || s.starts_with('"')) { s[1..s.len() - 1].to_string() } else { s.to_string() };
    let several = "A body of several statements goes inside BEGIN ... END, and nothing may follow its END but a ;.";
    let mut stack: Vec<(Blk, Option<String>)> = Vec::new();
    // At the first word of a statement, where BEGIN, IF and the rest open blocks.
    let mut start = true;
    // The statement after FOR EACH ROW is over: a `;` ended a plain one or an END closed the outermost block.
    let mut closed = false;
    let mut semi = false;
    let mut end = first.1.start;
    let mut k = 0;
    while k < toks.len() {
        let (t, r) = (toks[k].0, toks[k].1.clone());
        if closed {
            if punct(k, ";") && !semi {
                semi = true;
                k += 1;
                continue;
            }
            return Err(several.into());
        }
        if punct(k, ";") {
            match stack.last() {
                None => (closed, semi) = (true, true),
                Some((Blk::CaseExpr, _)) => return Err("A CASE expression is not closed before the ;.".into()),
                Some(_) => start = true,
            }
            k += 1;
            continue;
        }
        end = r.end;
        let is_word = t == Tok::Word && !after_dot(k);
        let up = if is_word { text(k).to_ascii_uppercase() } else { String::new() };
        if start {
            start = false;
            let mut label = None;
            let mut j = k;
            if matches!(t, Tok::Word | Tok::Quoted) && punct(k + 1, ":") {
                label = Some(unquote(text(k)).to_lowercase());
                j = k + 2;
            }
            let opener = match () {
                _ if word(j, "BEGIN") => Some(Blk::Begin),
                _ if word(j, "IF") && label.is_none() => Some(Blk::If),
                _ if word(j, "CASE") && label.is_none() => Some(Blk::CaseStmt),
                _ if word(j, "LOOP") => Some(Blk::Loop),
                _ if word(j, "WHILE") => Some(Blk::While),
                _ if word(j, "REPEAT") => Some(Blk::Repeat),
                _ if word(j, "FOR") => Some(Blk::For),
                _ => None,
            };
            if let Some(b) = opener {
                stack.push((b, label));
                // A block of statements starts one at once; IF, CASE, WHILE and FOR have a condition first.
                start = matches!(b, Blk::Begin | Blk::Loop | Blk::Repeat);
                end = toks[j].1.end;
                k = j + 1;
                continue;
            }
            if label.is_some() {
                return Err("A label goes before BEGIN, LOOP, WHILE or REPEAT.".into());
            }
            // DECLARE ... HANDLER FOR conditions: the handler's own statement starts after the conditions.
            if word(k, "DECLARE") {
                let mut j = k + 1;
                while j < toks.len() && !punct(j, ";") && !word(j, "HANDLER") {
                    j += 1;
                }
                if word(j, "HANDLER") && word(j + 1, "FOR") {
                    j += 2;
                    loop {
                        if word(j, "SQLSTATE") {
                            j += 1 + usize::from(word(j + 1, "VALUE"));
                            j += 1;
                        } else if word(j, "NOT") {
                            j += 2;
                        } else {
                            j += 1;
                        }
                        if punct(j, ",") {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    if j <= toks.len() {
                        end = toks[j - 1].1.end;
                        start = true;
                        k = j;
                        continue;
                    }
                }
            }
        }
        if is_word && up == "CASE" {
            stack.push((Blk::CaseExpr, None));
        } else if is_word && up == "END" {
            let Some((top, label)) = stack.pop() else { return Err(several.into()) };
            let next_kw = ["IF", "CASE", "LOOP", "WHILE", "REPEAT", "FOR"].into_iter().find(|w| word(k + 1, w));
            if next_kw != top.end_word() {
                let want = top.end_word().map_or("END".to_string(), |w| format!("END {w}"));
                return Err(format!("The blocks of the body do not match: {} where {want} was expected.", next_kw.map_or("END".to_string(), |w| format!("END {w}"))));
            }
            k += 1 + usize::from(next_kw.is_some());
            end = toks[k - 1].1.end;
            if top != Blk::CaseExpr {
                // An end label must repeat the begin label, so only that word is taken as one.
                if let Some(l) = &label {
                    if k < toks.len() && matches!(toks[k].0, Tok::Word | Tok::Quoted) && unquote(text(k)).to_lowercase() == *l {
                        end = toks[k].1.end;
                        k += 1;
                    }
                }
                closed = stack.is_empty();
            }
            continue;
        } else if is_word && matches!(up.as_str(), "THEN" | "ELSE") && matches!(stack.last(), Some((Blk::If | Blk::CaseStmt, _))) {
            start = true;
        } else if is_word && up == "DO" && matches!(stack.last(), Some((Blk::While | Blk::For, _))) {
            start = true;
        }
        k += 1;
    }
    if let Some((b, _)) = stack.last() {
        return Err(format!("The body has {} with no {} to close it.", match b {
            Blk::Begin => "a BEGIN",
            Blk::If => "an IF",
            Blk::CaseStmt | Blk::CaseExpr => "a CASE",
            Blk::Loop => "a LOOP",
            Blk::While => "a WHILE",
            Blk::Repeat => "a REPEAT",
            Blk::For => "a FOR",
        }, b.end_word().map_or("END".to_string(), |w| format!("END {w}"))));
    }
    let out = body[first.1.start..end].trim();
    if out.is_empty() || out == ";" {
        return Err("The trigger body is empty.".into());
    }
    Ok(out.to_string())
}

/// Where a trigger fires among the triggers of its table with the same timing and event.
#[derive(Clone, Debug, PartialEq)]
pub enum TrigOrder {
    Follows(String),
    Precedes(String),
}

/// The triggers just before and just after `name` in its timing and event, by ACTION_ORDER.
pub fn neighbours(all: &[Trigger], name: &str) -> (Option<String>, Option<String>) {
    let Some(me) = all.iter().find(|t| same_name(&t.name, name)) else { return (None, None) };
    let group = all.iter().filter(|t| !same_name(&t.name, name) && t.timing.eq_ignore_ascii_case(&me.timing) && t.event.eq_ignore_ascii_case(&me.event));
    let before = group.clone().filter(|t| t.order < me.order).max_by_key(|t| t.order).map(|t| t.name.clone());
    let after = group.filter(|t| t.order > me.order).min_by_key(|t| t.order).map(|t| t.name.clone());
    (before, after)
}

/// The clause that puts a trigger back between its old neighbours, among the triggers `live` that are there now:
/// after the one before it, else before the one after it, else none (it is the only one, or both are gone).
pub fn order_among(live: &[Trigger], name: &str, timing: &str, event: &str, before: Option<&str>, after: Option<&str>) -> Option<TrigOrder> {
    let here = |n: &str| live.iter().any(|t| !same_name(&t.name, name) && same_name(&t.name, n) && t.timing.eq_ignore_ascii_case(timing) && t.event.eq_ignore_ascii_case(event));
    match (before, after) {
        (Some(b), _) if here(b) => Some(TrigOrder::Follows(b.to_string())),
        (_, Some(a)) if here(a) => Some(TrigOrder::Precedes(a.to_string())),
        _ => None,
    }
}

fn order_clause(d: Dialect, o: Option<&TrigOrder>) -> String {
    match o {
        Some(TrigOrder::Follows(n)) => format!(" FOLLOWS {}", d.quote_ident(n)),
        Some(TrigOrder::Precedes(n)) => format!(" PRECEDES {}", d.quote_ident(n)),
        None => String::new(),
    }
}

/// `DEFINER=`user`@`host`` from information_schema's `user@host`. The host never holds an @, so the last one splits.
pub fn definer_clause(d: Dialect, definer: &str) -> Result<String, String> {
    let (user, host) = definer.rsplit_once('@').ok_or_else(|| format!("The definer {definer:?} cannot be written back."))?;
    Ok(format!("DEFINER={}@{}", d.quote_ident(user), d.quote_ident(host)))
}

/// How a trigger is saved: the editor's definition, the definer it keeps (an edited trigger stays its owner's) and
/// its place among the triggers of its timing and event.
#[derive(Clone, Debug, PartialEq)]
pub struct TrigSave {
    pub def: TrigDef,
    pub definer: Option<String>,
    pub order: Option<TrigOrder>,
}

/// The CREATE TRIGGER statement. `replace` writes MariaDB's CREATE OR REPLACE, which swaps an existing trigger in
/// one statement.
pub fn create_trigger(d: Dialect, m: Mode, table: &str, s: &TrigSave, replace: bool) -> Result<String, String> {
    let t = &s.def;
    check_ident(&t.name, "A trigger")?;
    let timing = t.timing.trim().to_ascii_uppercase();
    let event = t.event.trim().to_ascii_uppercase();
    if !matches!(timing.as_str(), "BEFORE" | "AFTER") {
        return Err("Timing is BEFORE or AFTER.".into());
    }
    if !matches!(event.as_str(), "INSERT" | "UPDATE" | "DELETE") {
        return Err("The event is INSERT, UPDATE or DELETE.".into());
    }
    let body = check_body(&t.body, m)?;
    let head = if replace { "CREATE OR REPLACE" } else { "CREATE" };
    let definer = match &s.definer {
        Some(x) => format!(" {}", definer_clause(d, x)?),
        None => String::new(),
    };
    Ok(format!("{head}{definer} TRIGGER {} {timing} {event} ON {}\nFOR EACH ROW{}\n{body}", d.quote_ident(&t.name), d.quote_ident(table), order_clause(d, s.order.as_ref())))
}

/// Checks a whole CREATE TRIGGER text again where it runs, under the session's own quote rules: the head up to FOR
/// EACH ROW and its order clause holds no `;` and no executable comment, and the rest is one body by `check_body`.
pub fn one_trigger_statement(sql: &str, m: Mode) -> Result<(), String> {
    use super::lex::{count_with, tokens_with, Tok};
    let toks: Vec<_> = tokens_with(sql, super::Dialect::MySql, m.bs, m.ansi).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    let w = |k: usize, s: &str| toks.get(k).is_some_and(|(t, r)| *t == Tok::Word && sql[r.clone()].eq_ignore_ascii_case(s));
    let at = (0..toks.len()).find(|&k| w(k, "FOR") && w(k + 1, "EACH") && w(k + 2, "ROW")).ok_or("The statement has no FOR EACH ROW.")?;
    let mut k = at + 3;
    if w(k, "FOLLOWS") || w(k, "PRECEDES") {
        k += 2;
    }
    let head = &sql[..toks.get(k).map_or(sql.len(), |(_, r)| r.start)];
    if toks[..k.min(toks.len())].iter().any(|(t, r)| matches!(t, Tok::ExecOpen | Tok::ExecClose) || (*t == Tok::Punct && &sql[r.clone()] == ";")) || count_with(head, super::Dialect::MySql, m.bs, m.ansi) != 1 {
        return Err("The trigger statement is not one statement.".into());
    }
    let body = toks.get(k).map_or("", |(_, r)| &sql[r.start..]);
    check_body(body, m).map(|_| ())
}

/// A stored trigger statement made ready to run again: MariaDB's OR REPLACE after CREATE, and the order clause after
/// FOR EACH ROW in place of one the text may already hold. The text is read with the quote rules of the mode it was
/// stored with.
pub fn restate(d: Dialect, stmt: &str, m: Mode, order: Option<&TrigOrder>, replace: bool) -> Result<String, String> {
    use super::lex::{tokens_with, Tok};
    let toks: Vec<_> = tokens_with(stmt, d, m.bs, m.ansi).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    let w = |k: usize, s: &str| toks.get(k).is_some_and(|(t, r)| *t == Tok::Word && stmt[r.clone()].eq_ignore_ascii_case(s));
    if !w(0, "CREATE") {
        return Err("The stored trigger does not start with CREATE.".into());
    }
    let at = (0..toks.len()).find(|&k| w(k, "FOR") && w(k + 1, "EACH") && w(k + 2, "ROW")).ok_or("The stored trigger has no FOR EACH ROW.")?;
    let row_end = toks[at + 2].1.end;
    let rest_at = if w(at + 3, "FOLLOWS") || w(at + 3, "PRECEDES") { toks.get(at + 4).map_or(stmt.len(), |(_, r)| r.end) } else { row_end };
    let create_end = toks[0].1.end;
    let or_replace = if replace && !w(1, "OR") { " OR REPLACE" } else { "" };
    Ok(format!("{}{or_replace}{}{}{}", &stmt[..create_end], &stmt[create_end..row_end], order_clause(d, order), &stmt[rest_at..]))
}

/// Whether two sql_mode values hold the same modes, in any order.
pub fn same_modes(a: &str, b: &str) -> bool {
    let set = |s: &str| {
        let mut v: Vec<String> = s.split(',').map(|p| p.trim().to_ascii_uppercase()).filter(|p| !p.is_empty()).collect();
        v.sort();
        v
    };
    set(a) == set(b)
}

/// Whether the live trigger is the one a change left or an editor saw: every part that decides what it does, its
/// owner and its place.
pub fn same_trigger(live: &Trigger, want: &Trigger) -> bool {
    same_name(&live.name, &want.name) && live.timing.eq_ignore_ascii_case(&want.timing) && live.event.eq_ignore_ascii_case(&want.event) && live.body.trim() == want.body.trim() && live.definer == want.definer && same_modes(&live.sql_mode, &want.sql_mode) && live.order == want.order
}

/// A trigger as the server stores it, enough to make it again exactly: SHOW CREATE TRIGGER's statement, the session
/// it was made in (its sql_mode decides how the body runs, and the character set how its strings read), and the
/// triggers it sat between.
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub stmt: String,
    pub sql_mode: String,
    pub charset: String,
    pub collation: String,
    pub timing: String,
    pub event: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// What a trigger change did, for its undo.
#[derive(Clone, Debug, PartialEq)]
pub struct TrigUndo {
    pub name: String,
    /// The trigger to put back, or None to drop the one the change created.
    pub restore: Option<Stored>,
    /// The trigger as the change left it, or None when the change dropped it.
    pub left: Option<Trigger>,
    pub label: String,
}

/// A trigger change for the worker.
#[derive(Clone, Debug)]
pub enum TrigJob {
    Create(TrigSave),
    /// `was` is the trigger as the editor read it; the change is refused when the server's differs.
    Edit { was: Trigger, save: TrigSave },
    Drop(Trigger),
}

/// What Save sends for the draft `def`: a new trigger, or the live one `was` replaced by it. An edited trigger keeps
/// its definer, and its place among the triggers of its timing and event when those stay the same.
pub fn save_job(all: &[Trigger], was: Option<&Trigger>, def: &TrigDef) -> TrigJob {
    let Some(was) = was else { return TrigJob::Create(TrigSave { def: def.clone(), definer: None, order: None }) };
    let kept = was.timing.eq_ignore_ascii_case(&def.timing) && was.event.eq_ignore_ascii_case(&def.event);
    let order = kept.then(|| neighbours(all, &was.name)).and_then(|(before, after)| before.map(TrigOrder::Follows).or(after.map(TrigOrder::Precedes)));
    let definer = (!was.definer.is_empty()).then(|| was.definer.clone());
    TrigJob::Edit { was: was.clone(), save: TrigSave { def: def.clone(), definer, order } }
}

/// The statements as the preview shows them. A MySQL edit also creates the new trigger under a scratch name first and
/// holds a write lock on the table while it swaps them, which the note under the preview says.
pub fn trigger_preview(d: Dialect, m: Mode, table: &str, job: &TrigJob) -> Result<Vec<String>, String> {
    Ok(match job {
        TrigJob::Create(s) => vec![create_trigger(d, m, table, s, false)?],
        TrigJob::Edit { save, .. } if matches!(d, Dialect::MariaDb) => vec![create_trigger(d, m, table, save, true)?],
        TrigJob::Edit { was, save } => vec![drop_trigger(d, &was.name, false), create_trigger(d, m, table, save, false)?],
        TrigJob::Drop(t) => vec![drop_trigger(d, &t.name, false)],
    })
}

pub fn drop_trigger(d: Dialect, name: &str, if_exists: bool) -> String {
    format!("DROP TRIGGER {}{}", if if_exists { "IF EXISTS " } else { "" }, d.quote_ident(name))
}

/// The statements as the preview shows them.
pub fn script(steps: &[String]) -> String {
    steps.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n\n")
}

/// The note for an error the server gives when the login may not create a trigger for another definer.
fn definer_note(err: String, definer: Option<&str>) -> String {
    match definer {
        Some(who) if err.contains("1227") || err.contains("SET_USER_ID") || err.contains("SUPER") => format!("{err}. The trigger belongs to {who}, and keeping that owner needs the SET_USER_ID (or SUPER) privilege, which this login lacks. Nothing was changed."),
        _ => err,
    }
}

/// A writable connection whose session reads text the way the statements were written for. When the server's
/// sql_mode changed since, nothing runs and the error carries the mode now in force.
pub(super) async fn session(db: &Db, schema: &str, read_only: bool, m: Mode) -> Result<sqlx::MySqlConnection, (String, Option<String>)> {
    let mut c = super::write::writer_conn(db, schema, read_only).await.and_then(|c| c.into_my()).map_err(|x| (x, None))?;
    let now = session_sql_mode(&mut c).await.map_err(|x| (x, None))?;
    if Mode::of(&now) != m {
        return Err((format!("Nothing ran: the server's sql_mode is now {now:?}, which reads quotes or backslashes differently from when the statement was written."), Some(now)));
    }
    Ok(c)
}

/// One statement, prepared, so the server parses exactly one whatever the text holds.
pub(super) async fn exec(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<()> {
    let t = Instant::now();
    let res = sqlx::query(sql).persistent(false).execute(&mut *c).await;
    crate::db::note_here(sql, t, &res);
    res.map_err(se)?;
    Ok(())
}

pub(super) fn undo_action(conn: u64, schema: &str, table: &str, u: AlterUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: String::new(), cols: Vec::new(), undo: super::write::SqlUndo::Alter(Box::new(u)) } }
}

/// Runs a recorded undo after checking the table is still as the change left it. The answer is the same as for
/// applying one; the action comes back for another try only when nothing ran.
pub(super) async fn undo(db: &Db, a: UndoAction, u: AlterUndo, read_only: bool) -> Msg {
    if db.dialect == Dialect::Postgres {
        return super::ddl_pg::undo(db, a, u, read_only).await;
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
        let mut c = super::write::writer_conn(db, &schema, read_only).await?.into_my()?;
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
async fn stored(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, all: &[Trigger], t: &Trigger) -> Res<Stored> {
    let show = format!("SHOW CREATE TRIGGER {}", d.qualified(schema, &t.name));
    let row = raw_first(c, &show).await?.ok_or_else(|| format!("Could not read the definition of {}.", t.name))?;
    let col = |i: usize| super::worker::text_at(&row, i);
    let stmt = col(2).ok_or_else(|| format!("Could not read the definition of {}.", t.name))?;
    let (before, after) = neighbours(all, &t.name);
    Ok(Stored { stmt, sql_mode: col(1).unwrap_or_default(), charset: col(3).unwrap_or_default(), collation: col(4).unwrap_or_default(), timing: t.timing.clone(), event: t.event.clone(), before, after })
}

/// Sets the session's sql_mode, client character set and connection collation, each bound as a value.
async fn set_session(c: &mut sqlx::MySqlConnection, mode: &str, charset: &str, collation: &str) -> Res<()> {
    sqlx::query("SET SESSION sql_mode = ?").bind(mode).execute(&mut *c).await.map_err(se)?;
    sqlx::query("SET SESSION character_set_client = ?").bind(charset).execute(&mut *c).await.map_err(se)?;
    sqlx::query("SET SESSION collation_connection = ?").bind(collation).execute(&mut *c).await.map_err(se)?;
    Ok(())
}

/// Runs a stored trigger statement in the session it was made in, so its body reads and runs as before (a
/// PIPES_AS_CONCAT `||` stays a concatenation, an ANSI_QUOTES `"x"` a name), then puts the session back. The text
/// goes out as UTF-8, so a non-UTF-8 client character set is kept only for an ASCII statement; the collation, which
/// decides what its strings are, is always the stored one.
async fn replay(c: &mut sqlx::MySqlConnection, d: Dialect, s: &Stored, order: Option<&TrigOrder>, replace: bool) -> Res<()> {
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
async fn raw_first(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<Option<sqlx::mysql::MySqlRow>> {
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
async fn run_text(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<()> {
    raw_first(c, sql).await.map(|_| ())
}

/// The trigger named `name` among `all`.
fn find<'a>(all: &'a [Trigger], name: &str) -> Option<&'a Trigger> {
    all.iter().find(|t| same_name(&t.name, name))
}

/// A scratch name for trying a new trigger before the old one is dropped.
fn scratch_name() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("quarry_check_{:x}", n & 0xffff_ffff_ffff)
}

/// MySQL cannot replace a trigger in place, so the old one is dropped and the new one created while the table is
/// write-locked, which keeps every row write from slipping between the two. The new trigger is first created under a
/// scratch name and dropped again, so a body or definer the server refuses stops before the old one is touched.
/// When the CREATE still fails, `back` is put back. UNLOCK TABLES runs on every way out.
async fn swap_locked(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, table: &str, name: &str, try_first: Option<(&str, &str)>, create: Swap<'_>, back: &Stored, back_order: Option<&TrigOrder>) -> Res<()> {
    run_text(c, &format!("LOCK TABLES {} WRITE", d.qualified(schema, table))).await.map_err(|x| format!("{x}. Replacing a trigger on MySQL locks the table so no row is written without one, which needs the LOCK TABLES privilege. Nothing was changed."))?;
    let res = swap_inner(c, d, name, try_first, create, back, back_order).await;
    let unlocked = run_text(c, "UNLOCK TABLES").await;
    res?;
    unlocked
}

/// What a locked swap creates: our own CREATE text, or a stored trigger replayed in its session.
enum Swap<'a> {
    Text(&'a str),
    Stored(&'a Stored, Option<&'a TrigOrder>),
}

async fn swap_inner(c: &mut sqlx::MySqlConnection, d: Dialect, name: &str, try_first: Option<(&str, &str)>, create: Swap<'_>, back: &Stored, back_order: Option<&TrigOrder>) -> Res<()> {
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
async fn trigger_job(db: &Db, schema: &str, table: &str, job: &TrigJob, m: Mode, read_only: bool) -> Result<(String, TrigUndo), (String, Option<String>)> {
    let d = db.dialect;
    let mut c = session(db, schema, read_only, m).await?;
    let fail = |x: String| (x, None);
    let all = super::structure::triggers_on(&mut c, d, schema, table).await.map_err(fail)?;
    let changed = |n: &str| format!("Nothing ran: the trigger {n} changed on the server since it was read. Reload the structure and try again.");
    match job {
        TrigJob::Create(s) => {
            let sql = create_trigger(d, m, table, s, false).map_err(fail)?;
            one_trigger_statement(&sql, m).map_err(fail)?;
            if find(&all, &s.def.name).is_some() {
                return Err(fail(format!("The table already has a trigger called {}.", s.def.name)));
            }
            run_text(&mut c, &sql).await.map_err(fail)?;
            let live = super::structure::triggers_on(&mut c, d, schema, table).await.ok().and_then(|v| find(&v, &s.def.name).cloned());
            Ok((s.def.name.clone(), TrigUndo { name: s.def.name.clone(), restore: None, left: live, label: format!("created trigger {}", s.def.name) }))
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
            let left = super::structure::triggers_on(&mut c, d, schema, table).await.ok().and_then(|v| find(&v, &save.def.name).cloned());
            Ok((was.name.clone(), TrigUndo { name: was.name.clone(), restore: Some(old), left, label: format!("changed trigger {}", was.name) }))
        }
        TrigJob::Drop(was) => {
            let Some(live) = find(&all, &was.name).filter(|l| same_trigger(l, was)) else { return Err(fail(changed(&was.name))) };
            let old = stored(&mut c, d, schema, &all, live).await.map_err(fail)?;
            run_text(&mut c, &drop_trigger(d, &was.name, false)).await.map_err(fail)?;
            Ok((was.name.clone(), TrigUndo { name: was.name.clone(), restore: Some(old), left: None, label: format!("dropped trigger {}", was.name) }))
        }
    }
}

/// Puts a trigger back as a change found it. It runs only when the trigger is as the change left it, or absent
/// where a previous undo stopped halfway, so it never overwrites a newer change; done twice it changes nothing.
async fn undo_trigger_job(db: &Db, schema: &str, table: &str, u: &TrigUndo, read_only: bool) -> Res<String> {
    let d = db.dialect;
    let mut c = super::write::writer_conn(db, schema, read_only).await?.into_my()?;
    let all = super::structure::triggers_on(&mut c, d, schema, table).await?;
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
pub(super) async fn undo_trigger(db: &Db, a: UndoAction, u: TrigUndo, read_only: bool) -> Msg {
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let res = crate::db::sql_scope(db.conn, &schema, undo_trigger_job(db, &schema, &table, &u, read_only)).await;
    let keep = res.is_err().then(|| UndoAction { at, ..trigger_undo_action(conn, &schema, &table, u) });
    Msg::SqlAltered { conn, database: String::new(), schema, table, res, undo: keep, sql_mode: None, alter: false, unsure: false }
}

fn trigger_undo_action(conn: u64, schema: &str, table: &str, u: TrigUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: String::new(), cols: Vec::new(), undo: super::write::SqlUndo::Trigger(Box::new(u)) } }
}

/// The count of orphan rows on PostgreSQL and a few of them, from reads of their own.
async fn pg_orphans(db: &Db, schema: &str, table: &str, fk: &ForeignKey) -> (Res<u64>, Vec<String>) {
    let n = match db.meta(schema, &orphans_sql(db.dialect, schema, table, fk), []).await {
        Ok(rows) => rows.first().and_then(|r| super::structure::num(r, "n")).unwrap_or(0),
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
pub(super) fn orphan_row(r: &super::exec::SqlRow, n: usize) -> String {
    format!("({})", (0..n).map(|i| super::worker::text(r, &format!("k{i}")).unwrap_or_else(|| "NULL".into())).collect::<Vec<_>>().join(", "))
}

impl Worker {
    /// Saves, replaces or drops a trigger written for the sql_mode `m`. The definition it replaces is read first with
    /// SHOW CREATE TRIGGER, with the session it was made in: it is what a refused MySQL swap and undo put back.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_trigger(&self, conn: u64, db: Db, schema: String, table: String, job: TrigJob, m: Mode, read_only: bool, done: String) {
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
            let res = res.map_err(se).map(|r| super::structure::num(&r, "n").unwrap_or(0));
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
                assert_eq!(lex::split_statements(&sql, Dialect::MySql).len(), 1, "{n} bs={bs}: {sql}");
                // Every backtick that is not a doubled one of an identifier closes an identifier we opened.
                assert!(lex::read_only_check(&sql, Dialect::MySql).is_err());
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
        assert_eq!(lex::count_with(&sql, Dialect::MySql, true, false), 1, "{sql}");
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

    /// Final review finding 6: an undo kept its steps in the mode of the change, and a session whose sql_mode changed
    /// since refused it with "Nothing ran" forever. The same inverse is now written again for the new mode.
    #[test]
    fn an_undo_is_written_again_for_a_changed_sql_mode() {
        let mut t = table();
        let i = t.columns.iter().position(|c| c.name == "total").unwrap();
        t.columns[i].comment = "C:\\temp".into();
        let c = [Change::Drop("total".into())];
        let u = plan(M, "8.4.0", B, "orders", &t, &c).unwrap().undo.unwrap();
        let nb = plan(M, "8.4.0", NB, "orders", &t, &c).unwrap().undo.unwrap().steps;
        assert!(u.steps[0].contains("'C:\\\\temp'") && nb[0].contains("'C:\\temp'"), "{:?} {nb:?}", u.steps);
        assert_eq!(u.steps_for(M, "8.4.0", "orders", B).unwrap(), u.steps);
        assert_eq!(u.steps_for(M, "8.4.0", "orders", NB).unwrap(), nb);
        // One that cannot be written for the new mode says so; the worker then drops the entry.
        assert!(AlterUndo { groups: vec![Vec::new()], ..u }.steps_for(M, "8.4.0", "orders", NB).is_err());
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
        ForeignKey { name: name.into(), db: "shop".into(), table: "orders".into(), cols: cols.iter().map(|c| (*c).into()).collect(), ref_db: "shop".into(), ref_table: rt.into(), ref_cols: rc.iter().map(|c| (*c).into()).collect(), on_update: "CASCADE".into(), on_delete: "RESTRICT".into(), ..Default::default() }
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
        assert!(fk_problem(&own, "orders", &parent, &k, false).is_none());
        let set_null = ForeignKey { on_delete: "SET NULL".into(), ..k.clone() };
        assert!(fk_problem(&own, "orders", &parent, &set_null, false).unwrap().contains("allows NULL"));
        assert!(fk_problem(&own, "orders", &parent, &ForeignKey { cols: vec!["opt".into()], ..set_null }, false).is_none());
        let unindexed = Structure { indexes: vec![], ..parent.clone() };
        assert!(fk_problem(&own, "orders", &unindexed, &k, false).unwrap().contains("no index"));
        assert!(fk_problem(&own, "orders", &parent, &ForeignKey { cols: vec!["gone".into()], ..k }, false).is_some());
    }

    #[test]
    fn postgres_keys_need_types_the_server_compares_and_a_unique_parent() {
        let pgcol = |n: &str, t: &str| Column { name: n.into(), full_type: t.into(), ..fcol(n, t, None) };
        let p = |a: &str, b: &str| fk_pair_problem_pg("orders", &pgcol("c", a), "customers", &pgcol("id", b));
        // every integer size meets every other, and an integer meets a numeric, text meets varchar
        assert!(p("integer", "bigint").is_none() && p("smallint", "integer").is_none() && p("integer", "numeric(10,2)").is_none());
        assert!(p("character varying(40)", "text").is_none() && p("date", "timestamp with time zone").is_none());
        // a different family is named with its types
        for (a, b) in [("integer", "text"), ("uuid", "integer"), ("boolean", "integer"), ("bytea", "text"), ("character(3)", "integer")] {
            assert!(p(a, b).unwrap().contains("can compare"), "{a} and {b}");
        }
        // character(n) and text are compared through the implicit cast, so the server takes a key between them
        assert!(p("text", "character(3)").is_none() && p("character(3)", "text").is_none() && p("character(3)", "character varying(9)").is_none());
        // an integer child against a numeric parent is taken, the other way round the server says it "cannot be implemented"
        assert!(p("integer", "numeric(10,2)").is_none() && p("numeric(10,2)", "integer").unwrap().contains("can compare") && p("numeric", "bigint").is_some());
        // a domain, an enum or an extension type is the server's to judge
        assert!(p("my_domain", "integer").is_none() && p("mood", "mood").is_none());
        let own = Structure { columns: vec![pgcol("a", "integer"), Column { nullable: true, ..pgcol("b", "integer") }, pgcol("c", "text")], pg_schema: Some("public".into()), ..Default::default() };
        let key = |name: &str, cols: &[&str]| Index { name: name.into(), unique: true, cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), ..Default::default() };
        let parent = Structure { columns: vec![pgcol("id", "integer"), pgcol("code", "integer"), pgcol("note", "text")], indexes: vec![key("p_pkey", &["id"]), key("p_pair", &["id", "code"]), Index { unique: false, ..key("p_note", &["note"]) }], pg_schema: Some("public".into()), ..Default::default() };
        let k = fk("k", &["a"], "p", &["id"]);
        assert!(fk_problem(&own, "orders", &parent, &k, false).is_none());
        // the referenced columns must be exactly a unique key, in any order, and a plain index does not count
        assert!(fk_problem(&own, "orders", &parent, &fk("k", &["a", "b"], "p", &["code", "id"]), false).is_none());
        assert!(fk_problem(&own, "orders", &parent, &fk("k", &["a"], "p", &["code"]), false).unwrap().contains("exactly (code)"));
        assert!(fk_problem(&own, "orders", &parent, &fk("k", &["c"], "p", &["note"]), false).unwrap().contains("primary key or unique"));
        let deferrable = Structure { indexes: vec![Index { deferrable: true, ..key("p_pkey", &["id"]) }], ..parent.clone() };
        assert!(fk_problem(&own, "orders", &deferrable, &k, false).is_some(), "a deferrable unique constraint cannot be referenced");
        assert!(fk_problem(&own, "orders", &parent, &fk("k", &["a", "a"], "p", &["id", "code"]), false).unwrap().contains("twice"));
        let set_null = ForeignKey { on_delete: "SET NULL".into(), ..k.clone() };
        assert!(fk_problem(&own, "orders", &parent, &set_null, false).unwrap().contains("allows NULL"));
        // MySQL's word for a key on part of a key does not reach PostgreSQL, where the whole key is always needed
        assert!(fk_problem(&own, "orders", &parent, &k, true).is_none());
    }

    #[test]
    fn the_rows_that_stop_a_key_are_selected_as_text() {
        let k = ForeignKey { ref_db: "o\"b".into(), ..fk("n", &["a", "b"], "p", &["x", "y"]) };
        assert_eq!(orphan_sample_sql(Dialect::Postgres, "sh", "t", &k, 5), "SELECT * FROM (SELECT c.\"a\"::text AS k0, c.\"b\"::text AS k1 FROM \"sh\".\"t\" c WHERE c.\"a\" IS NOT NULL AND c.\"b\" IS NOT NULL AND NOT EXISTS (SELECT 1 FROM \"o\"\"b\".\"p\" p WHERE p.\"x\" = c.\"a\" AND p.\"y\" = c.\"b\") LIMIT 5) s");
        assert!(orphans_sql(Dialect::Postgres, "sh", "t", &k).starts_with("SELECT COUNT(*) AS n FROM \"sh\".\"t\" c WHERE"));
    }

    #[test]
    fn a_match_full_key_also_counts_the_rows_that_are_part_null() {
        let k = ForeignKey { match_full: true, ref_db: String::new(), ..fk("n", &["a", "b"], "p", &["x", "y"]) };
        let count = orphans_sql(Dialect::Postgres, "sh", "t", &k);
        // MATCH SIMPLE leaves a row with a NULL part alone, MATCH FULL refuses it unless every part is NULL
        assert!(count.contains("(c.\"a\" IS NULL OR c.\"b\" IS NULL) AND (c.\"a\" IS NOT NULL OR c.\"b\" IS NOT NULL)"), "{count}");
        assert!(count.contains("NOT EXISTS (SELECT 1 FROM \"sh\".\"p\" p WHERE p.\"x\" = c.\"a\" AND p.\"y\" = c.\"b\")"), "{count}");
        assert!(orphan_sample_sql(Dialect::Postgres, "sh", "t", &k, 5).contains("IS NULL OR"));
        // a key of one column, or MATCH SIMPLE, is counted as before
        let simple = ForeignKey { match_full: false, ..k.clone() };
        assert!(!orphans_sql(Dialect::Postgres, "sh", "t", &simple).contains("IS NULL"));
        assert!(!orphans_sql(Dialect::Postgres, "sh", "t", &ForeignKey { match_full: true, ..fk("n", &["a"], "p", &["x"]) }).contains("IS NULL"));
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

    fn save(def: TrigDef) -> TrigSave {
        TrigSave { def, definer: None, order: None }
    }

    #[test]
    fn trigger_statements_by_engine() {
        let t = trig("orders_bu", "SET NEW.tag = 'x';");
        let live = crate::db::sql::structure::Trigger { name: "orders_bu".into(), ..Default::default() };
        let create = TrigJob::Create(save(t.clone()));
        let edit = TrigJob::Edit { was: live, save: TrigSave { definer: Some("app@%".into()), order: Some(TrigOrder::Follows("orders_first".into())), ..save(t.clone()) } };
        assert_eq!(trigger_preview(M, B, "orders", &create).unwrap(), ["CREATE TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW\nSET NEW.tag = 'x'"]);
        // The definer and the place of an edited trigger show in the preview, as they are sent.
        assert_eq!(trigger_preview(M, B, "orders", &edit).unwrap(), ["DROP TRIGGER `orders_bu`", "CREATE DEFINER=`app`@`%` TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW FOLLOWS `orders_first`\nSET NEW.tag = 'x'"]);
        // MariaDB replaces in place, so an edit is one statement and a new trigger is a plain CREATE.
        assert_eq!(trigger_preview(Dialect::MariaDb, B, "orders", &edit).unwrap(), ["CREATE OR REPLACE DEFINER=`app`@`%` TRIGGER `orders_bu` BEFORE UPDATE ON `orders`\nFOR EACH ROW FOLLOWS `orders_first`\nSET NEW.tag = 'x'"]);
        assert!(trigger_preview(Dialect::MariaDb, B, "orders", &create).unwrap()[0].starts_with("CREATE TRIGGER "));
        assert_eq!(drop_trigger(M, "a`b", false), "DROP TRIGGER `a``b`");
        assert_eq!(drop_trigger(M, "a", true), "DROP TRIGGER IF EXISTS `a`");
        assert_eq!(script(&["A".into(), "B".into()]), "A;\n\nB;");
        let p = create_trigger(M, B, "orders", &TrigSave { order: Some(TrigOrder::Precedes("n`x".into())), ..save(t) }, false).unwrap();
        assert!(p.contains("FOR EACH ROW PRECEDES `n``x`\n"), "{p}");
    }

    #[test]
    fn trigger_names_are_quoted_and_the_body_stays_the_users() {
        for bs in [true, false] {
            let t = TrigDef { name: "t`; DROP TABLE x; --".into(), timing: "AFTER".into(), event: "DELETE".into(), body: "BEGIN\n  INSERT INTO log VALUES ('a''b');\nEND".into() };
            let s = create_trigger(M, Mode { bs, ansi: false }, "or`ders", &save(t), false).unwrap();
            assert_eq!(s, "CREATE TRIGGER `t``; DROP TABLE x; --` AFTER DELETE ON `or``ders`\nFOR EACH ROW\nBEGIN\n  INSERT INTO log VALUES ('a''b');\nEND", "bs={bs}");
        }
        let bad = |t: TrigDef| create_trigger(M, B, "orders", &save(t), false);
        assert!(bad(trig("", "SET NEW.a = 1")).is_err());
        assert!(bad(TrigDef { timing: "INSTEAD OF".into(), ..trig("a", "SET NEW.a = 1") }).is_err());
        assert!(bad(TrigDef { event: "TRUNCATE".into(), ..trig("a", "SET NEW.a = 1") }).is_err());
        assert!(bad(trig("a", "  ")).is_err());
        assert!(bad(trig("a", " ; ")).is_err());
        // The definer is quoted part by part, split at its last @.
        assert_eq!(definer_clause(M, "we`ird@user@%").unwrap(), "DEFINER=`we``ird@user`@`%`");
        assert!(definer_clause(M, "nohost").is_err());
    }

    #[test]
    fn a_second_statement_in_a_trigger_body_is_refused() {
        assert_eq!(check_body("SET NEW.a = 1;", B).unwrap(), "SET NEW.a = 1");
        assert!(check_body("SET NEW.a = 1; DROP TABLE users", B).is_err());
        assert_eq!(check_body("-- note\nBEGIN SET NEW.a = 1; SET NEW.b = 2; END;", B).unwrap(), "BEGIN SET NEW.a = 1; SET NEW.b = 2; END");
        assert!(check_body("begin\nset new.a = 1;\nend", B).is_ok());
        // The same text is one statement or two depending on whether a backslash escapes the quote.
        let tricky = r"SET NEW.a = 'x\'; DROP TABLE users; --'";
        assert!(check_body(tricky, B).is_ok());
        assert!(check_body(tricky, NB).is_err());
        // Under ANSI_QUOTES a double quoted ; is an identifier, and either way it is not a statement end.
        assert!(check_body(r#"SET NEW.a = "x;y""#, B).is_ok());
        assert!(check_body(r#"SET NEW.a = "x;y""#, Mode { bs: true, ansi: true }).is_ok());
    }

    /// Review finding 1: a body that starts with BEGIN was taken whole, so a statement after its END ran too.
    #[test]
    fn nothing_runs_after_the_end_of_a_compound_body() {
        for body in [
            "BEGIN END; DROP TABLE quarry_rt.rt_victim",
            "BEGIN SET NEW.tag='new'; END; DELIMITER ;",
            "BEGIN END DROP TABLE x",
            "BEGIN END;;",
            // A name after a dot is never a keyword, so it opens no block for a later END to close.
            "BEGIN SET NEW.case = 1; END; DROP TABLE x; END",
            "BEGIN SET NEW.begin = 1; SET NEW.if = 2; END; DROP TABLE x; END",
            // A versioned comment may be skipped by the server, which would see the END after it early.
            "BEGIN /*!99999 BEGIN */ END; DROP TABLE x; END",
            "BEGIN /*M! BEGIN */ END; DROP TABLE x; END",
            // An end label is only the begin label, and a label never hides the END that follows.
            "a: BEGIN END a; DROP TABLE x",
            "BEGIN END END",
            "BEGIN IF 1 THEN SET NEW.a = 1; END; DROP TABLE x; END IF; END",
            "IF 1 THEN SET NEW.a = 1; END IF; DROP TABLE x",
            "BEGIN SET NEW.a = CASE WHEN 1 THEN 2 END; END; DROP TABLE x",
        ] {
            assert!(check_body(body, B).is_err(), "{body}");
            assert!(check_body(body, NB).is_err(), "{body}");
        }
        // Where the string ends depends on the escaping mode, and the check follows the session's.
        let s = r"BEGIN SET NEW.t = 'x\'; END; DROP TABLE y; -- '; END";
        assert!(check_body(s, B).is_ok());
        assert!(check_body(s, NB).is_err());
        let q = r#"BEGIN SET NEW.t = "a; END; DROP TABLE x; "; END"#;
        assert!(check_body(q, B).is_ok());
        assert!(check_body(q, Mode { bs: true, ansi: true }).is_ok());
        // Blocks that do not match, or one left open, are refused.
        for body in ["BEGIN IF 1 THEN SET NEW.a = 1; END; END", "BEGIN SET NEW.a = 1;", "IF 1 THEN SET NEW.a = 1; END LOOP", "BEGIN CASE NEW.a WHEN 1 THEN SET NEW.b = 1; END; END", "lbl: IF 1 THEN SET NEW.a = 1; END IF"] {
            assert!(check_body(body, B).is_err(), "{body}");
        }
        // The whole statement is checked again where it runs, head included.
        let ok = create_trigger(M, B, "t", &save(trig("a", "BEGIN SET NEW.a = 1; END")), false).unwrap();
        assert!(one_trigger_statement(&ok, B).is_ok());
        assert!(one_trigger_statement(&format!("{ok}; DROP TABLE x"), B).is_err());
        assert!(one_trigger_statement("CREATE TRIGGER a BEFORE INSERT ON t; DROP TABLE x; FOR EACH ROW SET NEW.a = 1", B).is_err());
        assert!(one_trigger_statement("CREATE TRIGGER a BEFORE INSERT ON t FOR EACH ROW FOLLOWS b SET NEW.a = 1; DROP TABLE x", B).is_err());
    }

    /// Review finding 11: a compound statement without BEGIN and a labelled block were refused.
    #[test]
    fn every_kind_of_compound_body_is_taken() {
        for body in [
            "IF NEW.tag IS NULL THEN SET NEW.tag = 'n'; END IF",
            "IF NEW.tag IS NULL THEN SET NEW.tag = 'n'; ELSEIF NEW.tag = '' THEN SET NEW.tag = 'e'; ELSE SET NEW.tag = UPPER(NEW.tag); END IF;",
            "lbl: BEGIN SET NEW.tag = 'l'; END lbl",
            "`l b`: BEGIN SET NEW.tag = 'l'; END `l b`;",
            "CASE NEW.a WHEN 1 THEN SET NEW.b = 'one'; ELSE BEGIN END; END CASE",
            "BEGIN DECLARE i INT DEFAULT 0; w: WHILE i < 3 DO SET i = i + 1; END WHILE w; REPEAT SET i = i - 1; UNTIL i = 0 END REPEAT; l: LOOP LEAVE l; END LOOP l; END",
            "BEGIN DECLARE CONTINUE HANDLER FOR SQLSTATE VALUE '23000', NOT FOUND, 1062 BEGIN END; SET NEW.a = 1; END",
            "BEGIN DECLARE EXIT HANDLER FOR SQLEXCEPTION SET NEW.a = 0; IF NEW.b THEN SET NEW.a = 1; END IF; END",
            "SET NEW.a = CASE WHEN NEW.b > 0 THEN 1 ELSE CASE NEW.c WHEN 1 THEN 2 END END",
            "BEGIN SET NEW.a = IF(NEW.b, 1, 2); SET NEW.c = REPEAT('x', 2); SET NEW.`end` = 1; SET NEW.end = 2; END",
            "BEGIN IF (SELECT CASE WHEN 1 THEN 1 END) THEN SET NEW.a = 1; END IF; END",
        ] {
            assert!(check_body(body, B).is_ok(), "{body}: {:?}", check_body(body, B));
        }
        assert_eq!(check_body("lbl: BEGIN SET NEW.tag = 'l'; END lbl;  -- done", B).unwrap(), "lbl: BEGIN SET NEW.tag = 'l'; END lbl");
    }

    /// Review finding 12: the quote rules come from the exact mode names, as the session reports them.
    #[test]
    fn ansi_quotes_is_read_exactly() {
        assert!(Mode::of("STRICT_TRANS_TABLES,ANSI_QUOTES").ansi);
        assert!(Mode::of("REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ANSI").ansi);
        assert!(!Mode::of("STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION").ansi);
        assert!(!Mode::of("ANSI_QUOTES_NOT_A_MODE").ansi);
    }

    /// Review finding 2 and 3: a stored statement is run again with OR REPLACE on MariaDB and its place among the
    /// triggers of its event, whatever order clause it held.
    #[test]
    fn a_stored_trigger_is_restated() {
        let s = "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW SET NEW.tag = 'a' || 'b'";
        assert_eq!(restate(M, s, B, None, false).unwrap(), s);
        assert_eq!(restate(Dialect::MariaDb, s, B, Some(&TrigOrder::Follows("p".into())), true).unwrap(), "CREATE OR REPLACE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW FOLLOWS `p` SET NEW.tag = 'a' || 'b'");
        let held = "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW PRECEDES `old` SET NEW.tag = 'for each row'";
        assert_eq!(restate(M, held, B, Some(&TrigOrder::Precedes("n".into())), false).unwrap(), "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW PRECEDES `n` SET NEW.tag = 'for each row'");
        assert_eq!(restate(M, held, B, None, false).unwrap(), "CREATE DEFINER=`root`@`%` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW SET NEW.tag = 'for each row'");
        // Under ANSI_QUOTES a double quoted name is read as one.
        let ansi = "CREATE DEFINER=`root`@`%` TRIGGER \"for each row\" BEFORE INSERT ON `x` FOR EACH ROW SET NEW.\"tag\" = 'q'";
        assert!(restate(M, ansi, Mode { bs: true, ansi: true }, Some(&TrigOrder::Follows("p".into())), false).unwrap().ends_with("FOR EACH ROW FOLLOWS `p` SET NEW.\"tag\" = 'q'"));
        assert!(restate(M, "DROP TABLE x", B, None, false).is_err());
    }

    fn live_trig(name: &str, order: u64) -> crate::db::sql::structure::Trigger {
        crate::db::sql::structure::Trigger { name: name.into(), timing: "BEFORE".into(), event: "INSERT".into(), body: "SET NEW.a = 1".into(), definer: "root@%".into(), order, sql_mode: "STRICT_TRANS_TABLES".into() }
    }

    /// Review finding 5: a trigger goes back between the triggers it sat between.
    #[test]
    fn the_order_is_found_from_the_neighbours() {
        let all = [live_trig("a", 1), live_trig("b", 2), live_trig("c", 3), crate::db::sql::structure::Trigger { timing: "AFTER".into(), ..live_trig("z", 1) }];
        assert_eq!(neighbours(&all, "b"), (Some("a".into()), Some("c".into())));
        assert_eq!(neighbours(&all, "a"), (None, Some("b".into())));
        assert_eq!(neighbours(&all, "z"), (None, None));
        let rest = [live_trig("a", 1), live_trig("c", 2)];
        assert_eq!(order_among(&rest, "b", "BEFORE", "INSERT", Some("a"), Some("c")), Some(TrigOrder::Follows("a".into())));
        assert_eq!(order_among(&rest[1..], "b", "BEFORE", "INSERT", Some("a"), Some("c")), Some(TrigOrder::Precedes("c".into())));
        assert_eq!(order_among(&[], "b", "BEFORE", "INSERT", Some("a"), Some("c")), None);
        // The trigger itself, still there for MariaDB's OR REPLACE, is never its own neighbour.
        assert_eq!(order_among(&[live_trig("b", 1)], "b", "BEFORE", "INSERT", Some("b"), None), None);
    }

    /// Review finding 14: the trigger check compares the definer, the sql_mode and the order too.
    #[test]
    fn a_trigger_is_the_same_only_when_every_part_is() {
        let t = live_trig("Tr", 1);
        assert!(same_trigger(&crate::db::sql::structure::Trigger { name: "tr".into(), body: " SET NEW.a = 1 ".into(), sql_mode: "strict_trans_tables".into(), ..t.clone() }, &t));
        assert!(!same_trigger(&crate::db::sql::structure::Trigger { body: "SET NEW.a = 2".into(), ..t.clone() }, &t));
        assert!(!same_trigger(&crate::db::sql::structure::Trigger { definer: "app@%".into(), ..t.clone() }, &t));
        assert!(!same_trigger(&crate::db::sql::structure::Trigger { sql_mode: "ANSI_QUOTES".into(), ..t.clone() }, &t));
        assert!(!same_trigger(&crate::db::sql::structure::Trigger { order: 2, ..t.clone() }, &t));
        assert!(same_modes("A,B", " b,a "));
    }

    /// Review finding 10: MySQL 8.4 takes a key only on a whole primary or unique key.
    #[test]
    fn a_strict_server_needs_a_whole_unique_key() {
        let own = Structure { columns: vec![fcol("a", "int", None)], ..Default::default() };
        let mut parent = Structure { columns: vec![fcol("x", "int", None), fcol("y", "int", None)], indexes: vec![ix("idx_x", false, &["x"])], ..Default::default() };
        let k = fk("fk", &["a"], "p", &["x"]);
        assert!(fk_problem(&own, "c", &parent, &k, false).is_none());
        assert!(fk_problem(&own, "c", &parent, &k, true).unwrap().contains("whole primary or unique key"));
        parent.indexes = vec![ix("uq_xy", true, &["x", "y"])];
        assert!(fk_problem(&own, "c", &parent, &k, true).is_some(), "part of a unique key is not enough");
        parent.indexes = vec![ix("uq_x", true, &["x"])];
        assert!(fk_problem(&own, "c", &parent, &k, true).is_none());
        assert!(fk_whole_key_default(M, "8.4.11"));
        assert!(fk_whole_key_default(M, "9.1.0"));
        assert!(!fk_whole_key_default(M, "8.0.40"));
        assert!(!fk_whole_key_default(Dialect::MariaDb, "11.4.3-MariaDB"));
    }

    /// Review finding 4: foreign key changes in a batch take the same plan as the rest, so their undo runs one step
    /// at a time, is checked against a snapshot that holds the key, and names what it loses.
    #[test]
    fn a_batch_with_a_foreign_key_has_one_checked_undo() {
        let t = table();
        let k = fk("fk_mix", &["customer_id"], "customers", &["id"]);
        let u = plan(M, "8.4.0", B, "orders", &t, &[Change::Drop("note".into()), Change::AddFk(k.clone())]).unwrap().undo.unwrap();
        assert_eq!(u.steps.len(), 2, "{:?}", u.steps);
        assert_eq!(u.steps[0], "ALTER TABLE `orders`\n  DROP FOREIGN KEY `fk_mix`");
        assert!(u.steps[1].contains("ADD COLUMN `note`"), "{:?}", u.steps);
        assert_eq!(u.fks, ["fk_mix"]);
        let u = plan(M, "8.4.0", B, "orders", &t, &[Change::Add { def: def("qty", "INT"), pos: Pos::Keep }, Change::AddFk(k)]).unwrap().undo.unwrap();
        assert_eq!(u.loss, ["Undo drops column qty and every value it holds."], "a loss asks for the typed confirmation");
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
            let opts = db.opts.my().unwrap().clone();
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
        final_6_undo_after_the_sql_mode_changed(&mut r, &db);
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

    /// Final review finding 6: the change ran in the default mode, the undo in a session with NO_BACKSLASH_ESCAPES. It
    /// used to be refused with "Nothing ran" for good; now its steps are written again for that session.
    fn final_6_undo_after_the_sql_mode_changed(r: &mut R, db: &Db) {
        let t = "sf_mode_undo";
        r.fresh(t, &format!(r"CREATE TABLE {S}.{t} (id INT PRIMARY KEY, note VARCHAR(20) NULL DEFAULT 'C:\\temp' COMMENT 'it''s \\ here')"));
        let before = r.st(db, t);
        assert_eq!(col(&before, "note").default.as_deref(), Some(r"C:\temp"));
        let (res, undo, ..) = r.apply(db, t, &[Change::Drop("note".into())]);
        res.unwrap();
        let nb = r.moded("NO_BACKSLASH_ESCAPES");
        r.w.sql_undo(nb.clone(), undo.expect("an undo"), Vec::new(), false);
        let Msg::SqlAltered { res, undo, sql_mode, .. } = recv(&r.rx) else { panic!("expected SqlAltered") };
        res.unwrap_or_else(|e| panic!("{e}"));
        assert!(undo.is_none());
        assert!(sql_mode.is_some_and(|m| !Mode::of(&m).bs), "the answer carries the mode the undo was written for");
        let after = r.st(db, t);
        assert_eq!(col(&after, "note").default.as_deref(), Some(r"C:\temp"));
        assert_eq!(col(&after, "note").comment, r"it's \ here");
        assert_eq!(shape(&after), shape(&before));
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
        /// The sql_mode every connection of these tests starts with.
        mode: String,
    }

    impl Env {
        fn try_exec(&self, sql: &str) -> Result<(), String> {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.map(|_| ()).map_err(|e| e.to_string()) })
        }

        fn exec(&self, sql: &str) {
            self.try_exec(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        }

        fn scalar(&self, sql: &str) -> Option<String> {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).fetch_one(&self.pool).await.ok().and_then(|r| super::super::worker::text_at(&r, 0)) })
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

        /// Sends a trigger job as the app does, written for the mode the app last saw.
        fn job_on(&mut self, db: &Db, table: &str, job: TrigJob) -> (Res<String>, Option<UndoAction>, Option<String>) {
            let m = Mode::of(&self.db.sql_mode);
            self.w.sql_trigger(1, db.clone(), "quarry_rt".into(), table.into(), job, m, false, "done".into());
            let Msg::SqlAltered { res, undo, sql_mode, alter, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
            assert!(!alter, "a trigger answer is not an ALTER");
            (res, undo, sql_mode)
        }

        fn job(&mut self, table: &str, job: TrigJob) -> Result<UndoAction, String> {
            let db = self.db.clone();
            let (res, undo, _) = self.job_on(&db, table, job);
            res?;
            Ok(undo.expect("an undo"))
        }

        fn live_trigger(&mut self, table: &str, name: &str) -> Option<crate::db::sql::structure::Trigger> {
            self.structure(table).triggers.into_iter().find(|t| t.name == name)
        }

        /// Creates a new trigger, or replaces the one of that name as the editor does (keeping its definer and place).
        fn trigger(&mut self, table: &str, def: &TrigDef, exists: bool) -> Result<UndoAction, String> {
            let all = self.structure(table).triggers;
            let was = all.iter().find(|t| t.name == def.name).cloned();
            assert_eq!(was.is_some(), exists, "{}", def.name);
            self.job(table, save_job(&all, was.as_ref(), def))
        }

        fn drop_trigger(&mut self, table: &str, name: &str) -> Result<UndoAction, String> {
            let t = self.live_trigger(table, name).expect("a trigger to drop");
            self.job(table, TrigJob::Drop(t))
        }

        /// The tag a fresh row gets from the BEFORE INSERT triggers.
        fn tag(&self) -> Option<String> {
            self.try_exec("INSERT INTO quarry_rt.rt_trig (tag) VALUES (NULL)").map_err(|e| format!("INSERT FAILS: {e}")).err().or_else(|| self.scalar("SELECT tag FROM quarry_rt.rt_trig ORDER BY id DESC LIMIT 1"))
        }

        /// Runs `sql` on one connection of the test pool with `mode` added to its session, then puts the mode back.
        fn exec_in_mode(&self, mode: &str, sql: &str) {
            self.exec(&format!("SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',{mode}'); {sql}; SET SESSION sql_mode = '{}'", self.mode));
        }
    }

    /// A pool whose every connection starts in `mode`, whatever the server's global mode is at that moment.
    fn pinned(w: &Worker, opts: sqlx::mysql::MySqlConnectOptions, mode: &str) -> sqlx::MySqlPool {
        let set = format!("SET SESSION sql_mode = '{mode}'");
        w.rt.block_on(async move {
            sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(4)
                .after_connect(move |c, _| {
                    let set = set.clone();
                    Box::pin(async move { sqlx::Executor::execute(c, set.as_str()).await.map(|_| ()) })
                })
                .connect_with(opts)
                .await
                .unwrap()
        })
    }

    /// Connects the worker and a plain pool for the checks, both pinned to the server's mode without
    /// NO_BACKSLASH_ESCAPES: another live test sets the global mode for a moment, and a connection opened then would
    /// read text differently from the one the worker was connected with.
    fn start(var: &str) -> Option<Env> {
        let uri = std::env::var(var).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        w.sql_connect(1, uri.clone(), Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((mut db, ..)), .. } = recv(&rx) else { panic!("expected SqlConnected") };
        let mode = db.sql_mode.split(',').filter(|m| !m.trim().eq_ignore_ascii_case("NO_BACKSLASH_ESCAPES")).collect::<Vec<_>>().join(",");
        db.pool = Pool::My(pinned(&w, db.opts.my().unwrap().clone(), &mode));
        db.sql_mode = mode.clone();
        let p = pinned(&w, db.opts.my().unwrap().clone(), &mode);
        let pool = w.rt.block_on(async {
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
        Some(Env { w, rx, db, pool, n: 100, mode })
    }

    fn key(name: &str, table: &str, cols: &[&str], ref_db: &str, ref_table: &str, ref_cols: &[&str]) -> ForeignKey {
        ForeignKey { name: name.into(), db: "quarry_rt".into(), table: table.into(), cols: cols.iter().map(|c| (*c).into()).collect(), ref_db: ref_db.into(), ref_table: ref_table.into(), ref_cols: ref_cols.iter().map(|c| (*c).into()).collect(), on_update: "CASCADE".into(), on_delete: "RESTRICT".into(), ..Default::default() }
    }

    fn foreign_keys(var: &str) {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut e) = start(var) else { return };
        let before = shape(&e.structure("rt_orders"));
        let k = key("fk_rt_cust", "rt_orders", &["customer_id"], "quarry_rt", "rt_customers", &["id"]);
        let (own, parent) = (e.structure("rt_orders"), e.structure("rt_customers"));
        assert_eq!(fk_problem(&own, "rt_orders", &parent, &k, false), None, "{var}");

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
        assert_eq!(fk_problem(&own, "rt_orders", &parent, &x, false), None, "{var}");
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
        assert!(fk_problem(&own, "rt_orders", &parent, &bad, false).is_some());
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

        // A refused edit leaves the old trigger in place (MySQL tries it under a scratch name first).
        let bad = def("rt_bi", "SET NEW.nope_column = 1");
        if let Err(err) = e.trigger("rt_trig", &bad, true) {
            assert!(!err.is_empty());
            assert_eq!(e.tag().as_deref(), Some("b"), "{var}: the old trigger survived {err}");
            assert_eq!(body_of(&mut e, "rt_bi").as_deref(), Some("SET NEW.tag = 'b'"));
            assert!(!e.structure("rt_trig").triggers.iter().any(|t| t.name.starts_with("quarry_check_")), "{var}: no scratch trigger is left");
        } else {
            // MariaDB checks NEW columns when the trigger runs, not when it is made.
            assert_eq!(e.db.dialect, Dialect::MariaDb, "{var}: MySQL must refuse a missing column");
            e.trigger("rt_trig", &b, true).unwrap();
        }

        // Undo restores the old body.
        e.undo(edit_undo).unwrap_or_else(|er| panic!("{var}: {er}"));
        assert_eq!(e.tag().as_deref(), Some("a"), "{var}: undo restored the previous body");
        // Undo is refused when the trigger was changed by someone else.
        let again = e.trigger("rt_trig", &b, true).unwrap();
        e.exec("DROP TRIGGER quarry_rt.rt_bi");
        e.exec("CREATE TRIGGER quarry_rt.rt_bi BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'z'");
        let refused = e.undo(again).unwrap_err();
        assert!(refused.contains("changed after this change ran"), "{var}: {refused}");
        assert_eq!(e.tag().as_deref(), Some("z"), "{var}: a refused undo changes nothing");
        e.exec("DROP TRIGGER quarry_rt.rt_bi");
        // Undo of a create whose trigger is already gone changes nothing and says so.
        assert!(e.undo(create_undo).is_ok());

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
        e.w.sql_trigger(1, e.db.clone(), "quarry_rt".into(), "rt_trig".into(), TrigJob::Create(TrigSave { def: a.clone(), definer: None, order: None }), Mode::of(&e.db.sql_mode), true, "done".into());
        let Msg::SqlAltered { res, .. } = recv(&e.rx) else { panic!() };
        assert!(res.is_err());
        assert!(e.structure("rt_trig").triggers.is_empty());

        e.exec("DROP DATABASE quarry_rt");
        e.exec("DROP DATABASE quarry_rt_b");
    }

    /// The review of 49a4432 and c5c77f1, each finding with the input that proved it.
    fn review(var: &str) {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut e) = start(var) else { return };
        let mysql = e.db.dialect == Dialect::MySql;
        let def = |name: &str, body: &str| TrigDef { name: name.into(), timing: "BEFORE".into(), event: "INSERT".into(), body: body.into() };
        let exists = |e: &mut Env, name: &str| e.live_trigger("rt_trig", name).is_some();
        let clear = |e: &Env| e.exec("DELETE FROM quarry_rt.rt_trig");

        // Finding 1: a body that smuggles a statement after its END is refused before anything runs, also by the
        // worker when it is handed one.
        e.exec("CREATE TABLE quarry_rt.rt_victim (x INT)");
        for body in ["BEGIN END; DROP TABLE quarry_rt.rt_victim", "BEGIN /*!99999 BEGIN */ END; DROP TABLE quarry_rt.rt_victim; END"] {
            let (res, undo, _) = { let db = e.db.clone(); e.job_on(&db, "rt_trig", TrigJob::Create(TrigSave { def: def("rt_s", body), definer: None, order: None })) };
            assert!(res.is_err() && undo.is_none(), "{var}: {body}");
            assert!(!exists(&mut e, "rt_s"), "{var}: {body}");
        }
        assert_eq!(e.scalar("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'quarry_rt' AND TABLE_NAME = 'rt_victim'").as_deref(), Some("1"), "{var}");
        e.exec("CREATE TRIGGER quarry_rt.rt_n BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'old'");
        let r = e.trigger("rt_trig", &def("rt_n", "BEGIN SET NEW.tag='new'; END; DELIMITER ;"), true);
        assert!(r.is_err(), "{var}");
        assert_eq!(e.tag().as_deref(), Some("old"), "{var}: nothing changed on the server");
        e.exec("DROP TRIGGER quarry_rt.rt_n");
        // Finding 11: bodies the old check refused are saved and run.
        let u = e.trigger("rt_trig", &def("rt_if", "IF NEW.tag IS NULL THEN SET NEW.tag = 'n'; END IF"), false).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(e.tag().as_deref(), Some("n"));
        e.undo(u).unwrap();
        let u = e.trigger("rt_trig", &def("rt_lbl", "lbl: BEGIN SET NEW.tag = 'l'; END lbl"), false).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(e.tag().as_deref(), Some("l"));
        e.undo(u).unwrap();
        clear(&e);

        // Finding 2: a trigger made under PIPES_AS_CONCAT, edited and undone, still concatenates.
        e.exec_in_mode("PIPES_AS_CONCAT", "CREATE TRIGGER quarry_rt.rt_p BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'a' || 'b'");
        assert_eq!(e.tag().as_deref(), Some("ab"), "{var}");
        let u = e.trigger("rt_trig", &def("rt_p", "SET NEW.tag = 'x'"), true).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(e.tag().as_deref(), Some("x"));
        e.undo(u).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(e.tag().as_deref(), Some("ab"), "{var}: the undone trigger runs in its own mode");
        assert!(e.live_trigger("rt_trig", "rt_p").unwrap().sql_mode.contains("PIPES_AS_CONCAT"), "{var}");
        // And a dropped one comes back in it too.
        let u = e.drop_trigger("rt_trig", "rt_p").unwrap();
        e.undo(u).unwrap();
        assert_eq!(e.tag().as_deref(), Some("ab"), "{var}");
        e.exec("DROP TRIGGER quarry_rt.rt_p");

        // Finding 3: a trigger made under ANSI_QUOTES survives a refused edit and an edit and its undo.
        e.exec_in_mode("ANSI_QUOTES", "CREATE TRIGGER quarry_rt.rt_q BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.\"tag\" = 'q'");
        assert_eq!(e.tag().as_deref(), Some("q"));
        let refused = e.trigger("rt_trig", &def("rt_q", "SET NEW.nope = 1"), true);
        if mysql {
            assert!(refused.is_err(), "{var}");
        }
        assert!(exists(&mut e, "rt_q"), "{var}: the old trigger stays after a refused edit");
        if refused.is_ok() {
            e.exec("DROP TRIGGER quarry_rt.rt_q");
            e.exec_in_mode("ANSI_QUOTES", "CREATE TRIGGER quarry_rt.rt_q BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.\"tag\" = 'q'");
        }
        let u = e.trigger("rt_trig", &def("rt_q", "SET NEW.tag = 'y'"), true).unwrap();
        assert_eq!(e.tag().as_deref(), Some("y"));
        e.undo(u).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(e.tag().as_deref(), Some("q"), "{var}: the ANSI_QUOTES body is back");
        e.exec("DROP TRIGGER quarry_rt.rt_q");
        // An undo whose old trigger the server now refuses keeps a trigger: the edited one is put back.
        e.exec("ALTER TABLE quarry_rt.rt_trig ADD COLUMN extra INT NULL");
        e.exec("CREATE TRIGGER quarry_rt.rt_x BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.extra = 1, NEW.tag = 'old'");
        let u = e.trigger("rt_trig", &def("rt_x", "SET NEW.tag = 'new'"), true).unwrap();
        e.exec("ALTER TABLE quarry_rt.rt_trig DROP COLUMN extra");
        let r = e.undo(u);
        assert!(exists(&mut e, "rt_x"), "{var}: a trigger is left after the undo ({r:?})");
        if mysql {
            assert!(r.unwrap_err().contains("put back"), "{var}");
            assert_eq!(e.tag().as_deref(), Some("new"), "{var}: the edited trigger is the one put back");
        }
        e.exec("DROP TRIGGER quarry_rt.rt_x");
        // An undo that finds no trigger, as a run that stopped halfway leaves, makes the old one again.
        e.exec("CREATE TRIGGER quarry_rt.rt_h BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'h1'");
        let u = e.trigger("rt_trig", &def("rt_h", "SET NEW.tag = 'h2'"), true).unwrap();
        e.exec("DROP TRIGGER quarry_rt.rt_h");
        e.undo(u).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(e.tag().as_deref(), Some("h1"), "{var}");
        e.exec("DROP TRIGGER quarry_rt.rt_h");
        clear(&e);

        // Finding 5: two triggers that append keep their order through an edit, its undo, and a drop and its undo.
        e.exec("CREATE TRIGGER quarry_rt.rt_1 BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '1')");
        e.exec("CREATE TRIGGER quarry_rt.rt_2 BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '2')");
        assert_eq!(e.tag().as_deref(), Some("12"));
        let u = e.trigger("rt_trig", &def("rt_1", "SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '1') /* same */"), true).unwrap();
        assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after the edit");
        e.undo(u).unwrap();
        assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after its undo");
        let u = e.drop_trigger("rt_trig", "rt_1").unwrap();
        assert_eq!(e.tag().as_deref(), Some("2"));
        e.undo(u).unwrap();
        assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after the drop and its undo");
        let u = e.trigger("rt_trig", &def("rt_2", "SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '2') /* same */"), true).unwrap();
        assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after editing the last one");
        e.undo(u).unwrap();
        e.exec("DROP TRIGGER quarry_rt.rt_1");
        e.exec("DROP TRIGGER quarry_rt.rt_2");
        clear(&e);

        // Finding 6: an edited trigger keeps its definer, and a login that may not create one for another user is told
        // so and changes nothing.
        let _ = e.try_exec("DROP USER IF EXISTS 'qrf_owner'@'%'");
        let _ = e.try_exec("DROP USER IF EXISTS 'qrf_low'@'%'");
        e.exec("CREATE USER 'qrf_owner'@'%' IDENTIFIED BY 'x'");
        e.exec("CREATE USER 'qrf_low'@'%' IDENTIFIED BY 'qrf_low_pw'");
        e.exec("GRANT ALL ON quarry_rt.* TO 'qrf_owner'@'%'");
        e.exec("GRANT SELECT, INSERT, TRIGGER, LOCK TABLES ON quarry_rt.* TO 'qrf_low'@'%'");
        e.exec("CREATE DEFINER='qrf_owner'@'%' TRIGGER quarry_rt.rt_d BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'd'");
        let definer = |e: &Env| e.scalar("SELECT DEFINER FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = 'quarry_rt' AND TRIGGER_NAME = 'rt_d'");
        let u = e.trigger("rt_trig", &def("rt_d", "SET NEW.tag = 'e'"), true).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(definer(&e).as_deref(), Some("qrf_owner@%"), "{var}: the definer is kept");
        e.undo(u).unwrap();
        assert_eq!(definer(&e).as_deref(), Some("qrf_owner@%"));
        let low = {
            let pool = pinned(&e.w, e.db.opts.my().unwrap().clone().username("qrf_low").password("qrf_low_pw"), &e.mode);
            Db { pool: Pool::My(pool), ..e.db.clone() }
        };
        let all = e.structure("rt_trig").triggers;
        let was = all.iter().find(|t| t.name == "rt_d").cloned();
        let (res, _, _) = e.job_on(&low, "rt_trig", save_job(&all, was.as_ref(), &def("rt_d", "SET NEW.tag = 'f'")));
        let err = res.unwrap_err();
        assert!(err.contains("SET_USER_ID") && err.contains("Nothing was changed"), "{var}: {err}");
        assert_eq!(e.tag().as_deref(), Some("d"), "{var}: the old trigger stays");
        assert_eq!(definer(&e).as_deref(), Some("qrf_owner@%"));
        e.exec("DROP TRIGGER quarry_rt.rt_d");
        e.exec("DROP USER 'qrf_owner'@'%'");
        e.exec("DROP USER 'qrf_low'@'%'");
        clear(&e);

        // Finding 12: the worker reads the writer's own sql_mode, so a session in ANSI_QUOTES the app did not know of
        // runs nothing and hands the mode back.
        let ansi = Db { pool: Pool::My(pinned(&e.w, e.db.opts.my().unwrap().clone(), &format!("{},ANSI_QUOTES", e.mode))), ..e.db.clone() };
        let (res, _, seen) = e.job_on(&ansi, "rt_trig", TrigJob::Create(TrigSave { def: def("rt_m", r#"SET NEW.tag = "m""#), definer: None, order: None }));
        assert!(res.unwrap_err().contains("Nothing ran"), "{var}");
        assert!(seen.is_some_and(|m| Mode::of(&m).ansi), "{var}");
        assert!(!exists(&mut e, "rt_m"));

        // Finding 4: staged [Drop column wrong, AddFk fk_mix] takes the checked ALTER path: its undo is one ALTER per
        // step, refused when the key changed since, and run when it did not.
        let k = key("fk_mix", "rt_orders", &["customer_id"], "quarry_rt", "rt_customers", &["id"]);
        let before = shape(&e.structure("rt_orders"));
        let cols = e.structure("rt_orders").columns.len();
        let undo = e.apply("rt_orders", &[Change::Drop("wrong".into()), Change::AddFk(k)]).unwrap_or_else(|x| panic!("{var}: {x}"));
        let UndoKind::Sql { undo: crate::db::sql::write::SqlUndo::Alter(au), .. } = &undo.kind else { panic!("{var}: not the checked ALTER undo") };
        assert!(au.steps.len() >= 2 && au.fks == ["fk_mix"] && au.expect.iter().any(|(w, v)| w == "foreign key fk_mix" && v.is_some()), "{var}: {au:?}");
        e.exec("ALTER TABLE quarry_rt.rt_orders DROP FOREIGN KEY fk_mix");
        e.w.sql_undo(e.db.clone(), undo, Vec::new(), false);
        let Msg::SqlAltered { res, undo: kept, .. } = recv(&e.rx) else { panic!("expected SqlAltered") };
        let refused = res.unwrap_err();
        assert!(refused.contains("foreign key fk_mix changed"), "{var}: {refused}");
        let undo = kept.expect("a refused undo stays in the list");
        e.exec("ALTER TABLE quarry_rt.rt_orders ADD CONSTRAINT fk_mix FOREIGN KEY (customer_id) REFERENCES rt_customers (id) ON UPDATE CASCADE ON DELETE RESTRICT");
        e.undo(undo).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(shape(&e.structure("rt_orders")), before, "{var}");
        assert_eq!(e.structure("rt_orders").columns.len(), cols);

        // Finding 10: on MySQL 8.4 a key on part of a key is refused by the server (6125), and the pre-check says so first.
        e.n += 1;
        e.w.sql_fk_rules(e.n, e.db.clone());
        let Msg::SqlFkRules { strict, .. } = recv(&e.rx) else { panic!("expected SqlFkRules") };
        assert_eq!(strict, mysql && fk_whole_key_default(e.db.dialect, &e.db.version), "{var}");
        e.exec("CREATE TABLE quarry_rt.rt_loose (x INT NOT NULL, y INT NOT NULL, KEY idx_x (x))");
        e.exec("CREATE TABLE quarry_rt.rt_child (id INT PRIMARY KEY, x INT NULL)");
        let k = key("fk_loose", "rt_child", &["x"], "quarry_rt", "rt_loose", &["x"]);
        let (own, parent) = (e.structure("rt_child"), e.structure("rt_loose"));
        let pre = fk_problem(&own, "rt_child", &parent, &k, strict);
        let ran = e.apply("rt_child", &[Change::AddFk(k)]).map(|_| ());
        assert_eq!(pre.is_some(), ran.is_err(), "{var}: the pre-check agrees with the server ({pre:?}, {ran:?})");
        if strict {
            assert!(ran.unwrap_err().contains("6125"), "{var}");
        }

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

    #[test]
    fn live_review_mysql() {
        review("QUARRY_TEST_MYSQL");
    }

    #[test]
    fn live_review_mariadb() {
        review("QUARRY_TEST_MARIADB");
    }
}
