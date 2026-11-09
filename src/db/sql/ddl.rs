//! ALTER TABLE for the Structure view: column and index changes are staged as `Change`s, several of
//! them make one statement, and the statement that undoes them is worked out from the structure read
//! before applying. Everything here is pure text building. Names go through `quote_ident`, strings
//! through `lit`, and the few places that take SQL from the user (an expression default) are checked.

use super::structure::{split_type, Column, Index, Structure};
use super::value::lit;
use super::{Db, Dialect};
use crate::db::{e, Msg, Res, UndoAction, UndoKind, Worker};
use sqlx::Executor;
use std::collections::HashSet;
use std::time::Instant;

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
    /// "12,2" or "'a','b'": a length list, or the values of an ENUM or SET.
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
}

impl ColDef {
    pub fn new(name: &str, ty: &str) -> ColDef {
        ColDef { name: name.into(), ty: ty.into(), args: String::new(), unsigned: false, zerofill: false, nullable: true, default: Dv::Null, auto_increment: false, collation: None, on_update: None, comment: String::new() }
    }

    /// What the structure read holds about a column. Generated and invisible columns carry attributes
    /// that are not read, so redefining them would lose them: those are refused.
    pub fn from_column(c: &Column) -> Result<ColDef, String> {
        let extra = c.extra.to_ascii_uppercase().replace("DEFAULT_GENERATED", "");
        if extra.contains("GENERATED") || extra.contains("INVISIBLE") || extra.contains("PERSISTENT") {
            return Err(format!("Column {} is generated or invisible. Edit it with a query.", c.name));
        }
        let (ty, args, unsigned) = split_type(&c.full_type);
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
        Ok(ColDef { name: c.name.clone(), ty, args, unsigned, zerofill, nullable: c.nullable, default, auto_increment: lower.contains("auto_increment"), collation: c.collation.clone(), on_update, comment: c.comment.clone() })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pos {
    /// A new column goes last; an existing one stays where it is.
    Keep,
    First,
    After(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IndexKind {
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

#[derive(Clone, Debug, PartialEq)]
pub struct IndexDef {
    pub name: String,
    pub kind: IndexKind,
    pub cols: Vec<IdxCol>,
    pub comment: String,
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
        IndexDef { name: ix.name.clone(), kind, cols: ix.cols.iter().map(|c| IdxCol { name: c.name.clone(), prefix: c.prefix, desc: c.desc }).collect(), comment: ix.comment.clone() }
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
/// second statement and no comment that could swallow the rest of the ALTER.
pub fn check_expr(s: &str, bs: bool) -> Result<(), String> {
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
                i += 1;
                loop {
                    match b.get(i) {
                        None => return Err("The default expression has an unclosed quote.".into()),
                        Some('\\') if bs && c != '`' => i += 2,
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

/// The values of an ENUM or SET written as 'a','b' with '' for a quote, or a backslash escape unless
/// `bs` is off.
pub fn parse_values(args: &str, bs: bool) -> Result<Vec<String>, String> {
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
                Some('\\') if bs => match b.get(i + 1) {
                    Some(&n) => {
                        v.push(n);
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

/// The values as the editor shows them: 'a','b'.
pub fn values_text(vals: &[String], bs: bool) -> String {
    vals.iter().map(|v| lit(v, bs)).collect::<Vec<_>>().join(",")
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
pub fn definition(d: Dialect, bs: bool, c: &ColDef) -> Result<String, String> {
    check_ident(&c.name, "A column")?;
    let ty = c.ty.trim().to_ascii_uppercase();
    if ty.is_empty() || !ty.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || !ty.as_bytes()[0].is_ascii_alphabetic() {
        return Err(format!("{:?} is not a column type.", c.ty));
    }
    let mut s = ty.clone();
    let args = c.args.trim();
    if !args.is_empty() {
        let inner = if matches!(ty.as_str(), "ENUM" | "SET") {
            let vals = parse_values(args, bs)?;
            values_text(&vals, bs)
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
        Dv::Value(v) if ty == "BIT" && !v.is_empty() && v.bytes().all(|b| b == b'0' || b == b'1') => s.push_str(&format!(" DEFAULT b'{v}'")),
        Dv::Value(v) => s.push_str(&format!(" DEFAULT {}", lit(v, bs))),
        Dv::Expr(x) => {
            check_expr(x, bs)?;
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
        check_expr(u, bs)?;
        s.push_str(&format!(" ON UPDATE {u}"));
    }
    if !c.comment.is_empty() {
        s.push_str(&format!(" COMMENT {}", lit(&c.comment, bs)));
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

fn add_index(d: Dialect, bs: bool, ix: &IndexDef) -> Result<String, String> {
    let cols = index_cols(d, ix)?;
    if matches!(ix.kind, IndexKind::Fulltext | IndexKind::Spatial) && ix.cols.iter().any(|c| c.prefix.is_some() || c.desc) {
        return Err("A FULLTEXT or SPATIAL index has no prefix lengths or sort order.".into());
    }
    let comment = if ix.comment.is_empty() { String::new() } else { format!(" COMMENT {}", lit(&ix.comment, bs)) };
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
    Ok(format!("ADD {kind}INDEX {} ({cols}){comment}", d.quote_ident(&ix.name)))
}

fn drop_index(d: Dialect, name: &str) -> String {
    if name == "PRIMARY" { "DROP PRIMARY KEY".into() } else { format!("DROP INDEX {}", d.quote_ident(name)) }
}

/// One clause of the ALTER, or two for a rename the server cannot do directly.
fn clause(d: Dialect, bs: bool, can_rename: bool, c: &Change) -> Result<Vec<String>, String> {
    Ok(match c {
        Change::Add { def, pos } => vec![format!("ADD COLUMN {} {}{}", d.quote_ident(&def.name), definition(d, bs, def)?, position(d, pos))],
        Change::Modify { from, def, pos } if *from == def.name => vec![format!("MODIFY {} {}{}", d.quote_ident(from), definition(d, bs, def)?, position(d, pos))],
        Change::Modify { from, def, pos } => vec![format!("CHANGE COLUMN {} {} {}{}", d.quote_ident(from), d.quote_ident(&def.name), definition(d, bs, def)?, position(d, pos))],
        Change::Drop(n) => vec![format!("DROP COLUMN {}", d.quote_ident(n))],
        Change::AddIndex(ix) => vec![add_index(d, bs, ix)?],
        Change::DropIndex(n) => vec![drop_index(d, n)],
        Change::RenameIndex { from, to, def } => {
            check_ident(to, "An index")?;
            if can_rename {
                vec![format!("RENAME INDEX {} TO {}", d.quote_ident(from), d.quote_ident(to))]
            } else {
                vec![drop_index(d, from), add_index(d, bs, &IndexDef { name: to.clone(), ..def.clone() })?]
            }
        }
    })
}

/// The statement for the staged changes, one clause per line. The table is unqualified: the writer's
/// session is switched to the schema first, which is what the preview shows.
pub fn alter(d: Dialect, version: &str, bs: bool, table: &str, changes: &[Change]) -> Result<String, String> {
    if changes.is_empty() {
        return Err("Nothing is staged.".into());
    }
    let can = d.can_rename_index(version);
    let mut lines = Vec::new();
    for c in changes {
        lines.extend(clause(d, bs, can, c)?);
    }
    Ok(format!("ALTER TABLE {}\n  {}", d.quote_ident(table), lines.join(",\n  ")))
}

/// The table as the changes leave it, tracked while their inverses are collected.
struct Sim {
    cols: Vec<(String, Option<ColDef>)>,
    idx: Vec<IndexDef>,
}

impl Sim {
    fn of(st: &Structure) -> Sim {
        Sim { cols: st.columns.iter().map(|c| (c.name.clone(), ColDef::from_column(c).ok())).collect(), idx: st.indexes.iter().map(IndexDef::from_index).collect() }
    }

    fn at(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|(n, _)| n == name)
    }

    /// Where a column is now, as the clause that puts it back there.
    fn pos_of(&self, i: usize) -> Pos {
        if i == 0 { Pos::First } else { Pos::After(self.cols[i - 1].0.clone()) }
    }

    fn insert(&mut self, col: (String, Option<ColDef>), pos: &Pos) {
        let at = match pos {
            Pos::Keep => self.cols.len(),
            Pos::First => 0,
            Pos::After(c) => self.at(c).map_or(self.cols.len(), |i| i + 1),
        };
        self.cols.insert(at.min(self.cols.len()), col);
    }

    fn rename_in_indexes(&mut self, from: &str, to: &str) {
        for ix in &mut self.idx {
            for c in ix.cols.iter_mut().filter(|c| c.name == from) {
                c.name = to.into();
            }
        }
    }

    /// Applies one change and returns the changes that undo it, in the order to run them.
    fn apply(&mut self, c: &Change) -> Result<Vec<Change>, String> {
        match c {
            Change::Add { def, pos } => {
                self.insert((def.name.clone(), Some(def.clone())), pos);
                Ok(vec![Change::Drop(def.name.clone())])
            }
            Change::Modify { from, def, pos } => {
                let i = self.at(from).ok_or_else(|| format!("Column {from} is not in the table."))?;
                let old_pos = self.pos_of(i);
                let (_, old) = self.cols.remove(i);
                let old = old.ok_or_else(|| format!("Column {from} cannot be restored by undo."));
                if *pos == Pos::Keep {
                    self.cols.insert(i, (def.name.clone(), Some(def.clone())));
                } else {
                    self.insert((def.name.clone(), Some(def.clone())), pos);
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
                let old = old.ok_or_else(|| format!("Column {name} cannot be restored by undo."));
                // The server shrinks every index that holds the column and drops those left empty, and undo has to
                // bring them back as they were.
                let mut back = vec![Change::Add { def: old?, pos }];
                let mut keep = Vec::new();
                for mut ix in std::mem::take(&mut self.idx) {
                    if !ix.cols.iter().any(|c| c.name == *name) {
                        keep.push(ix);
                        continue;
                    }
                    let whole = ix.clone();
                    ix.cols.retain(|c| c.name != *name);
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
                let i = self.idx.iter().position(|x| x.name == *name || (name == "PRIMARY" && x.kind == IndexKind::Primary)).ok_or_else(|| format!("Index {name} is not on the table."))?;
                Ok(vec![Change::AddIndex(self.idx.remove(i))])
            }
            Change::RenameIndex { from, to, def } => {
                let i = self.idx.iter().position(|x| x.name == *from).ok_or_else(|| format!("Index {from} is not on the table."))?;
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

/// The changes with the index edits the engine will not do by itself: MariaDB refuses to drop a column that an
/// index holds, so each such index is dropped first and added again without the column (or left out when the
/// column was its only one).
pub fn expand(d: Dialect, st: &Structure, changes: &[Change]) -> Vec<Change> {
    if d.drops_index_with_column() {
        return changes.to_vec();
    }
    let mut sim = Sim::of(st);
    let mut out = Vec::new();
    for c in changes {
        if let Change::Drop(col) = c {
            for ix in sim.idx.iter().filter(|ix| ix.cols.iter().any(|c| c.name == *col)) {
                out.push(Change::DropIndex(if ix.kind == IndexKind::Primary { "PRIMARY".into() } else { ix.name.clone() }));
                let mut rest = ix.clone();
                rest.cols.retain(|c| c.name != *col);
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

/// The ALTER that puts the table back, from the structure read before the changes ran. Err says why
/// there is none.
pub fn inverse(d: Dialect, version: &str, bs: bool, table: &str, before: &Structure, changes: &[Change]) -> Result<String, String> {
    let mut sim = Sim::of(before);
    let mut groups = Vec::new();
    for c in changes {
        groups.push(sim.apply(c)?);
    }
    let back: Vec<Change> = groups.into_iter().rev().flatten().collect();
    alter(d, version, bs, table, &back)
}

/// What the staged changes cost that undo cannot give back. Shown in the preview.
pub fn warnings(before: &Structure, changes: &[Change]) -> Vec<String> {
    let mut out = Vec::new();
    let sim = Sim::of(before);
    for c in changes {
        match c {
            Change::Drop(n) => out.push(format!("Dropping {n} deletes its values for good. Undo brings the column back empty.")),
            Change::Modify { from, def, .. } => {
                if let Some(Some(old)) = sim.at(from).map(|i| sim.cols[i].1.as_ref()) {
                    let shrinks = old.args != def.args || old.ty != def.ty || old.unsigned != def.unsigned || (old.nullable && !def.nullable);
                    if shrinks {
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
    let set: HashSet<&str> = taken.iter().map(String::as_str).collect();
    let first = format!("{base}_copy");
    if !set.contains(first.as_str()) {
        return first;
    }
    (2..).map(|n| format!("{first}{n}")).find(|n| !set.contains(n.as_str())).unwrap_or(first)
}

async fn run(db: &Db, schema: &str, sql: &str, read_only: bool) -> Res<()> {
    let mut c = super::write::writer(db, schema, read_only).await?;
    c.execute(sql).await.map_err(e)?;
    Ok(())
}

fn undo_action(conn: u64, schema: &str, table: &str, sql: String, label: String) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { cols: Vec::new(), undo: super::write::SqlUndo::Alter { sql, label } } }
}

/// Runs a recorded undo statement. The answer is the same as for applying one.
pub(super) async fn undo(db: &Db, a: UndoAction, sql: String, label: String, read_only: bool) -> Msg {
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let res = run(db, &schema, &sql, read_only).await.map(|_| "Undid the structure change".to_string());
    let keep = res.is_err().then(|| undo_action(conn, &schema, &table, sql, label));
    // Keeps the age of the original so a failed undo does not live longer.
    let keep = keep.map(|mut k| {
        k.at = at;
        k
    });
    Msg::SqlAltered { conn, schema, table, res, undo: keep }
}

impl Worker {
    /// Runs one ALTER TABLE on a writable connection of its own. `undo` is the statement that reverses it,
    /// when there is one, and is handed back with the answer.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_alter(&self, conn: u64, db: Db, schema: String, table: String, sql: String, undo: Option<(String, String)>, read_only: bool, done: String) {
        self.spawn(async move {
            let res = run(&db, &schema, &sql, read_only).await.map(|_| done);
            let undo = if res.is_ok() { undo.map(|(sql, label)| undo_action(conn, &schema, &table, sql, label)) } else { None };
            Msg::SqlAltered { conn, schema, table, res, undo }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::lex;
    use crate::db::sql::structure::IndexCol;

    const M: Dialect = Dialect::MySql;

    fn alt(c: &[Change]) -> String {
        alter(M, "8.4.0", true, "orders", c).unwrap()
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
        assert!(alter(M, "8.4.0", true, "orders", &[]).is_err());
    }

    #[test]
    fn indexes() {
        let i = IndexDef { name: "idx_a".into(), kind: IndexKind::Index, cols: vec![IdxCol { name: "a".into(), prefix: Some(10), desc: false }, IdxCol { name: "b".into(), prefix: None, desc: true }], comment: String::new() };
        assert_eq!(alt(&[Change::AddIndex(i.clone())]), "ALTER TABLE `orders`\n  ADD INDEX `idx_a` (`a`(10), `b` DESC)");
        let u = IndexDef { kind: IndexKind::Unique, cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], comment: "why".into(), ..i.clone() };
        assert_eq!(alt(&[Change::AddIndex(u)]), "ALTER TABLE `orders`\n  ADD UNIQUE INDEX `idx_a` (`a`) COMMENT 'why'");
        let f = IndexDef { kind: IndexKind::Fulltext, cols: vec![IdxCol { name: "t".into(), prefix: None, desc: false }], ..i.clone() };
        assert_eq!(alt(&[Change::AddIndex(f)]), "ALTER TABLE `orders`\n  ADD FULLTEXT INDEX `idx_a` (`t`)");
        let p = IndexDef { name: "PRIMARY".into(), kind: IndexKind::Primary, cols: vec![IdxCol { name: "id".into(), prefix: None, desc: false }], comment: String::new() };
        assert_eq!(alt(&[Change::AddIndex(p)]), "ALTER TABLE `orders`\n  ADD PRIMARY KEY (`id`)");
        assert_eq!(alt(&[Change::DropIndex("idx_a".into())]), "ALTER TABLE `orders`\n  DROP INDEX `idx_a`");
        assert_eq!(alt(&[Change::DropIndex("PRIMARY".into())]), "ALTER TABLE `orders`\n  DROP PRIMARY KEY");
        // A prefix on a FULLTEXT index is refused, and an index needs columns and a name.
        assert!(alter(M, "8.4.0", true, "t", &[Change::AddIndex(IndexDef { kind: IndexKind::Fulltext, ..i.clone() })]).is_err());
        assert!(alter(M, "8.4.0", true, "t", &[Change::AddIndex(IndexDef { cols: vec![], ..i.clone() })]).is_err());
        assert!(alter(M, "8.4.0", true, "t", &[Change::AddIndex(IndexDef { name: String::new(), ..i })]).is_err());
    }

    #[test]
    fn rename_index_by_server_version() {
        let def = IndexDef { name: "old".into(), kind: IndexKind::Unique, cols: vec![IdxCol { name: "a".into(), prefix: None, desc: false }], comment: String::new() };
        let c = [Change::RenameIndex { from: "old".into(), to: "new".into(), def }];
        assert_eq!(alter(M, "8.4.0", true, "t", &c).unwrap(), "ALTER TABLE `t`\n  RENAME INDEX `old` TO `new`");
        assert_eq!(alter(Dialect::MariaDb, "10.11.6-MariaDB", true, "t", &c).unwrap(), "ALTER TABLE `t`\n  RENAME INDEX `old` TO `new`");
        assert_eq!(alter(Dialect::MariaDb, "10.4.0-MariaDB", true, "t", &c).unwrap(), "ALTER TABLE `t`\n  DROP INDEX `old`,\n  ADD UNIQUE INDEX `new` (`a`)");
        assert_eq!(alter(M, "5.6.1", true, "t", &c).unwrap(), "ALTER TABLE `t`\n  DROP INDEX `old`,\n  ADD UNIQUE INDEX `new` (`a`)");
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
            definition(M, true, &d)
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
            definition(M, true, &d).is_err()
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
                let ixd = IndexDef { name: n.into(), kind: IndexKind::Index, cols: vec![IdxCol { name: n.into(), prefix: None, desc: false }], comment: n.into() };
                let c = [Change::Add { def: d.clone(), pos: Pos::After(n.into()) }, Change::Modify { from: n.into(), def: ColDef { name: format!("{n}2"), ..d }, pos: Pos::Keep }, Change::Drop(n.into()), Change::AddIndex(ixd), Change::DropIndex(n.into())];
                let sql = alter(M, "8.4.0", bs, n, &c).unwrap();
                assert_eq!(lex::split_statements(&sql).len(), 1, "{n} bs={bs}: {sql}");
                // Every backtick that is not a doubled one of an identifier closes an identifier we opened.
                assert!(lex::read_only_check(&sql).is_err());
            }
        }
        let mut d = def("c", "VARCHAR");
        d.args = "20".into();
        d.default = Dv::Value("a\\'; DROP TABLE x; --".into());
        d.comment = "it's \\".into();
        assert_eq!(definition(M, true, &d).unwrap(), "VARCHAR(20) NULL DEFAULT 'a\\\\''; DROP TABLE x; --' COMMENT 'it''s \\\\'");
        assert_eq!(definition(M, false, &d).unwrap(), "VARCHAR(20) NULL DEFAULT 'a\\''; DROP TABLE x; --' COMMENT 'it''s \\'");
        assert_eq!(M.quote_ident("a`b"), "`a``b`");
        assert_eq!(alter(M, "8", true, "a`b", &[Change::Drop("c`d".into())]).unwrap(), "ALTER TABLE `a``b`\n  DROP COLUMN `c``d`");
    }

    #[test]
    fn values_round_trip() {
        for bs in [true, false] {
            let vals = vec!["a".to_string(), "it's".to_string(), "x,y".to_string(), "back\\slash".to_string(), String::new()];
            assert_eq!(parse_values(&values_text(&vals, bs), bs).unwrap(), vals);
        }
        assert!(parse_values("a,b", true).is_err());
        assert!(parse_values("'a'x", true).is_err());
        assert!(parse_values("'a", true).is_err());
        assert_eq!(parse_values("'a' , 'b'", true).unwrap(), ["a", "b"]);
    }

    #[test]
    fn a_column_read_back_is_the_same_column() {
        let t = table();
        let total = ColDef::from_column(&t.columns[2]).unwrap();
        assert_eq!((total.ty.as_str(), total.args.as_str(), total.unsigned, total.nullable, &total.default, total.comment.as_str()), ("DECIMAL", "10,2", true, false, &Dv::Value("0.00".into()), "Incl. tax"));
        let id = ColDef::from_column(&t.columns[0]).unwrap();
        assert!(id.auto_increment && id.unsigned && id.default == Dv::None);
        let note = ColDef::from_column(&t.columns[3]).unwrap();
        assert_eq!(note.default, Dv::Null);
        assert_eq!(definition(M, true, &note).unwrap(), "VARCHAR(50) CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci NULL DEFAULT NULL");
        let mut g = col("g", "int", true, None);
        g.extra = "VIRTUAL GENERATED".into();
        assert!(ColDef::from_column(&g).is_err());
        let mut s = col("s", "datetime", false, Some("CURRENT_TIMESTAMP"));
        s.default_expr = true;
        s.extra = "DEFAULT_GENERATED on update CURRENT_TIMESTAMP".into();
        let sd = ColDef::from_column(&s).unwrap();
        assert_eq!((sd.default.clone(), sd.on_update.as_deref()), (Dv::Expr("CURRENT_TIMESTAMP".into()), Some("CURRENT_TIMESTAMP")));
        let z = ColDef::from_column(&col("z", "int(5) unsigned zerofill", false, None)).unwrap();
        assert!(z.zerofill && z.unsigned && z.args == "5");
    }

    #[test]
    fn undo_of_each_shape() {
        let t = table();
        let inv = |c: &[Change]| inverse(M, "8.4.0", true, "orders", &t, c).unwrap();
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
        let new_ix = IndexDef { name: "i2".into(), kind: IndexKind::Index, cols: vec![IdxCol { name: "id".into(), prefix: None, desc: false }], comment: String::new() };
        assert_eq!(inv(&[Change::AddIndex(new_ix)]), "ALTER TABLE `orders`\n  DROP INDEX `i2`");
        assert_eq!(inv(&[Change::DropIndex("uq_note".into())]), "ALTER TABLE `orders`\n  ADD UNIQUE INDEX `uq_note` (`note`)");
        assert_eq!(inv(&[Change::DropIndex("PRIMARY".into())]), "ALTER TABLE `orders`\n  ADD PRIMARY KEY (`id`)");
        let rn = Change::RenameIndex { from: "uq_note".into(), to: "uq_memo".into(), def: IndexDef::from_index(&t.indexes[2]) };
        assert_eq!(inv(&[rn]), "ALTER TABLE `orders`\n  RENAME INDEX `uq_memo` TO `uq_note`");
    }

    #[test]
    fn undo_of_a_combined_change_runs_backwards() {
        let t = table();
        let c = [Change::Add { def: def("qty", "INT"), pos: Pos::After("id".into()) }, Change::Modify { from: "qty".into(), def: def("amount", "INT"), pos: Pos::Keep }, Change::Drop("customer_id".into())];
        let back = inverse(M, "8.4.0", true, "orders", &t, &c).unwrap();
        let lines: Vec<&str> = back.lines().collect();
        assert!(lines[1].starts_with("  ADD COLUMN `customer_id`") && lines[1].ends_with("AFTER `amount`,"), "{back}");
        assert!(back.contains("CHANGE COLUMN `amount` `qty` INT NULL DEFAULT NULL") && back.contains("DROP COLUMN `qty`"), "{back}");
        assert!(back.find("CHANGE COLUMN `amount`").unwrap() < back.find("DROP COLUMN `qty`").unwrap());
        // No undo for a generated column or an unknown one.
        let mut g = table();
        g.columns[3].extra = "STORED GENERATED".into();
        assert!(inverse(M, "8.4.0", true, "orders", &g, &[Change::Drop("note".into())]).is_err());
        assert!(inverse(M, "8.4.0", true, "orders", &t, &[Change::Drop("nope".into())]).is_err());
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
        let back = inverse(Dialect::MariaDb, "11.4.0-MariaDB", true, "orders", &t, &e).unwrap();
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
    fn shape(st: &Structure) -> Vec<String> {
        let cols = st.columns.iter().map(|c| format!("col {}|{}|{}|{:?}|{}|{}|{:?}|{}", c.name, c.full_type, c.nullable, c.default, c.default_expr, c.extra, c.collation, c.comment));
        let idx = st.indexes.iter().map(|i| format!("idx {}|{}|{}|{}|{}", i.name, i.unique, i.cols.iter().map(|c| format!("{}:{:?}:{}", c.name, c.prefix, c.desc)).collect::<Vec<_>>().join(","), i.kind, i.comment));
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
            let bs = self.db.backslash_escapes();
            let changes = expand(d, &before, &changes);
            let sql = alter(d, &v, bs, "t", &changes).unwrap_or_else(|e| panic!("{label}: {e}"));
            let back = inverse(d, &v, bs, "t", &before, &changes).unwrap_or_else(|e| panic!("{label}: {e}"));
            self.w.sql_alter(1, self.db.clone(), "quarry_ddl".into(), "t".into(), sql.clone(), Some((back, label.into())), false, "done".into());
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
        let bs = e.db.backslash_escapes();
        e.round_trip("change enum values, default and collation", vec![Change::Modify { from: "kind".into(), def: ColDef { args: values_text(&["a".into(), "b".into(), "it's".into(), "back\\slash".into()], bs), default: Dv::Value("it's".into()), collation: Some("utf8mb4_bin".into()), ..kind }, pos: Pos::Keep }], |st| {
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

        let ix = |name: &str, kind: IndexKind, cols: &[(&str, Option<u64>)]| IndexDef { name: name.into(), kind, cols: cols.iter().map(|(n, p)| IdxCol { name: (*n).into(), prefix: *p, desc: false }).collect(), comment: String::new() };
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
        e.w.sql_alter(1, e.db.clone(), "quarry_ddl".into(), "t".into(), "ALTER TABLE `t` DROP COLUMN `note`".into(), None, true, "done".into());
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
