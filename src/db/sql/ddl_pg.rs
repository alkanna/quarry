//! The PostgreSQL text of a structure change. The staged `Change`s and their undo groups are the ones `ddl` builds for
//! MySQL; what differs is the statements. PostgreSQL has no one statement that alters a column whole, so a change is
//! written as the clauses that differ from the column as the simulation holds it (type with USING, nullability, default,
//! identity, comment), and an index is its own CREATE INDEX or constraint. Every name is quoted and qualified with the
//! table's schema, so the search_path never decides which table is meant. The two pieces of user text, a type and a
//! default expression, are checked with the PostgreSQL lexer to stay one clause.
//!
//! The text is written for one fixed session: every write transaction pins standard_conforming_strings on (`pg::write_locals`),
//! so a literal needs only its quotes doubled, and the plan sets the search_path to the table's schema before it reads the
//! table again and before every statement, the path the plan-time structure read used, so a type, function or sequence
//! outside the schema is printed with its schema and resolves to the same object when it is written back.
//!
//! DDL is transactional here, which MySQL's is not: a plan and its undo each run as one transaction that takes the
//! table's lock, reads the touched columns and indexes again and refuses to run when another session changed them.

use super::ddl::{fk_line, orphan_row, orphan_sample_sql, orphans_sql, ref_state, same_name, snapshot, touched, undo_action, undo_loss, AlterUndo, Change, ColDef, Dv, Expect, IndexDef, IndexKind, Mode, Plan, Pos, RefCheck, Sim, ORPHAN_SAMPLE};
use super::exec::Tx;
use super::lex::{every_reading, Tok};
use super::structure::{col_deps, columns, constraints, foreign_keys, indexes, load, Column, ForeignKey, Structure};
use super::value::{lit, Param};
use super::{se, write, Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use std::time::Instant;

const D: Dialect = Dialect::Postgres;

/// How long the plan waits for the table's lock. A session that holds it longer is named in the error, and nothing ran.
pub(super) const LOCK_WAIT: &str = "3s";

/// PostgreSQL cuts a longer name without a word, so the staged name would not be the one the server keeps.
const MAX_NAME: usize = 63;

pub(super) fn q(name: &str) -> String {
    D.quote_ident(name)
}

pub(super) fn name_ok(name: &str, what: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{what} needs a name."));
    }
    if name.len() > MAX_NAME || name.contains('\0') {
        return Err(format!("{what} name is longer than 63 bytes or holds a NUL, which PostgreSQL would cut or refuse."));
    }
    Ok(())
}

/// Whether a string, dollar quote or quoted name token ends where it was meant to. The lexer lets an unterminated one run
/// to the end of the text, and an ending quote that a backslash escapes is not one.
fn closed(tok: &str, escapes: bool) -> bool {
    if let Some(rest) = tok.strip_prefix('$') {
        let Some(end) = rest.find('$') else { return false };
        let mark = &tok[..end + 2];
        return tok.len() >= 2 * mark.len() && tok.ends_with(mark);
    }
    let b = tok.as_bytes();
    let Some(open) = b.iter().position(|c| matches!(c, b'\'' | b'"')) else { return false };
    let quote = b[open];
    let escapes = quote == b'\'' && (escapes || tok[..open].eq_ignore_ascii_case("e"));
    let mut j = open + 1;
    while j < b.len() {
        if escapes && b[j] == b'\\' {
            j += 2;
        } else if b[j] == quote {
            if b.get(j + 1) == Some(&quote) {
                j += 2;
            } else {
                return j + 1 == b.len();
            }
        } else {
            j += 1;
        }
    }
    false
}

/// What the text may hold. A default expression is any expression; a type is words, names, dots, brackets and a
/// parenthesised list, so it can neither carry a string nor start another clause with a comma.
#[derive(Clone, Copy, PartialEq)]
enum Text {
    Expr,
    /// A trigger's WHEN condition, read like an expression and worded for it.
    Cond,
    Type,
}

/// The text must stay one clause in every reading the server may give it: no `;`, no comment that could swallow the rest
/// of the statement, balanced brackets and quotes. Both readings of backslashes are tried, so a string that one of them
/// ends early cannot hide a statement in the other.
fn scan(s: &str, what: Text) -> Result<(), String> {
    let name = match what {
        Text::Expr => "default expression",
        Text::Cond => "WHEN condition",
        Text::Type => "type",
    };
    let one = format!("The {name} must be {}, without ; or comments.", if what == Text::Type { "a type name like numeric(10,2)" } else { "one expression" });
    if s.trim().is_empty() {
        return Err(format!("The {name} is empty."));
    }
    for (n, toks) in every_reading(s, D).iter().enumerate() {
        let mut depth = 0i32;
        for (tok, r) in toks {
            let t = &s[r.clone()];
            match tok {
                Tok::Comment | Tok::ExecOpen | Tok::ExecClose => return Err(one),
                // PostgreSQL before 16 reads `1$a$` as a number followed by a dollar quote, which opens a string this scan
                // sees as one word.
                Tok::Word if t.as_bytes().first().is_some_and(u8::is_ascii_digit) && t.contains('$') => return Err(format!("The {name} holds {t:?}, which some PostgreSQL versions read as a number followed by a dollar quote.")),
                Tok::Str if what == Text::Type => return Err(format!("The type {t} holds a string, which a type name never does.")),
                Tok::Str | Tok::Quoted => {
                    if !closed(t, n == 1) {
                        return Err(format!("The {name} has an unclosed quote."));
                    }
                }
                Tok::Punct => match t {
                    "(" | "[" => depth += 1,
                    ")" | "]" => {
                        depth -= 1;
                        if depth < 0 {
                            return Err(format!("The {name} closes a bracket it never opened."));
                        }
                    }
                    ";" => return Err(one),
                    "," if what == Text::Type && depth == 0 => return Err(one),
                    "," | "." => {}
                    _ if what == Text::Type => return Err(format!("The type holds {t:?}, which a type name never does.")),
                    _ => {}
                },
                _ => {}
            }
        }
        if depth != 0 {
            return Err(format!("The {name} has an unclosed bracket."));
        }
    }
    Ok(())
}

/// A default expression typed by the person must stay one expression. See `scan`.
pub fn check_expr_pg(s: &str) -> Result<(), String> {
    scan(s, Text::Expr)
}

/// A trigger's WHEN condition typed by the person: one expression, like a default. See `scan`.
pub fn check_cond_pg(s: &str) -> Result<(), String> {
    scan(s, Text::Cond)
}

/// A column type typed by the person must stay one type name. See `scan`.
pub fn check_type_pg(s: &str) -> Result<(), String> {
    scan(s, Text::Type)
}

/// The words of a type without its length list or array brackets, lower case: "character varying" for
/// "character varying(255)[]".
fn type_words(ty: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in ty.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

/// The serial type that stands for an integer type, for a column that gets its numbers from a sequence of its own.
fn serial_of(ty: &str) -> Option<&'static str> {
    match type_words(ty).as_str() {
        "smallint" | "int2" => Some("smallserial"),
        "integer" | "int" | "int4" => Some("serial"),
        "bigint" | "int8" => Some("bigserial"),
        _ => None,
    }
}

/// How a column gets its own numbers. A serial is an integer with the default `nextval('seq')` of a sequence the column
/// owns (`ColDef::serial`, set only from the catalog read); an identity is the standard form. Both read as auto increment,
/// and the serial keeps its default as an expression so changing it back puts the same default on. A `nextval` default
/// typed by the person names a sequence the column does not own, so it is no serial: ticking identity on it is refused.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Auto {
    No,
    Serial,
    Identity(bool),
}

fn auto_of(c: &ColDef) -> Auto {
    match (&c.default, c.auto_increment) {
        (_, false) => Auto::No,
        (Dv::Expr(x), true) if c.serial && x.trim_start().to_ascii_lowercase().starts_with("nextval(") => Auto::Serial,
        _ => Auto::Identity(c.identity_always),
    }
}

/// A column as the catalog read gives it. A stored generated column is refused: it has an expression this definition does
/// not hold, and rewriting it would lose it.
pub(super) fn col_def(c: &Column) -> Result<ColDef, String> {
    if c.extra.contains("GENERATED ALWAYS AS (") {
        return Err(format!("Column {} is a generated column. Edit it with a query.", c.name));
    }
    let default = match (&c.default, c.default_expr) {
        (Some(d), true) => Dv::Expr(d.clone()),
        (Some(d), false) => Dv::Value(d.clone()),
        (None, _) if c.nullable => Dv::Null,
        (None, _) => Dv::None,
    };
    let identity = c.extra.contains("IDENTITY");
    Ok(ColDef { default, auto_increment: c.extra.contains("auto_increment"), serial: c.extra.contains("auto_increment") && !identity, identity_always: identity && c.extra.contains("GENERATED ALWAYS AS IDENTITY"), collation: c.collation.clone(), nullable: c.nullable, comment: c.comment.clone(), ..ColDef::new(&c.name, &c.full_type) })
}

/// A collation as the structure read gives it: a bare name (in pg_catalog, or typed by the person), or a ready
/// `"schema"."name"` for one in another schema, which the catalog prints qualified because the plan's search_path would not
/// find it.
fn split_qualified(c: &str) -> Option<(String, String)> {
    fn quoted(s: &str) -> Option<(String, &str)> {
        let b = s.as_bytes();
        if b.first() != Some(&b'"') {
            return None;
        }
        let mut j = 1;
        while j < b.len() {
            if b[j] == b'"' {
                if b.get(j + 1) == Some(&b'"') {
                    j += 2;
                    continue;
                }
                return Some((s[1..j].replace("\"\"", "\""), &s[j + 1..]));
            }
            j += 1;
        }
        None
    }
    let (schema, rest) = quoted(c)?;
    let (name, rest) = quoted(rest.strip_prefix('.')?)?;
    rest.is_empty().then_some((schema, name))
}

fn collation_sql(c: &str) -> String {
    match split_qualified(c) {
        Some((s, n)) => format!("{}.{}", q(&s), q(&n)),
        None => q(c),
    }
}

fn collate(c: &Option<String>) -> String {
    c.as_ref().map_or(String::new(), |c| format!(" COLLATE {}", collation_sql(c)))
}

fn collation_ok(c: &str) -> Result<(), String> {
    match split_qualified(c) {
        Some((s, n)) => name_ok(&s, "A collation schema").and_then(|_| name_ok(&n, "A collation")),
        None => name_ok(c, "A collation"),
    }
}

/// Whether a column of this type can be an identity or a serial.
pub(super) fn identity_type(ty: &str) -> bool {
    serial_of(ty).is_some()
}

fn no_pos(pos: &Pos) -> Result<(), String> {
    match pos {
        Pos::Keep => Ok(()),
        _ => Err("PostgreSQL cannot place a column: a new column goes last and the order of the others never changes.".into()),
    }
}

/// The type text of a definition, checked.
fn type_of(def: &ColDef) -> Result<String, String> {
    if !def.args.trim().is_empty() {
        return Err("A PostgreSQL type carries its length in its name, like numeric(10,2).".into());
    }
    let t = def.ty.trim();
    check_type_pg(t)?;
    Ok(t.to_string())
}

fn has_default(d: &Dv) -> bool {
    matches!(d, Dv::Value(_) | Dv::Expr(_))
}

/// The `(expression)` or literal of a default, or None when the column has none. A literal is written unknown-typed, so
/// the server reads it as the column's type, which is also what it reports back. Its quotes are doubled and nothing else
/// is escaped, which is right because every write transaction pins standard_conforming_strings on.
fn default_of(c: &ColDef) -> Result<Option<String>, String> {
    Ok(match &c.default {
        Dv::None | Dv::Null => None,
        Dv::Value(v) if v.contains('\0') => return Err("A default cannot hold a NUL character.".into()),
        Dv::Value(v) => Some(lit(v, false)),
        Dv::Expr(x) => {
            check_expr_pg(x)?;
            Some(format!("({})", x.trim()))
        }
    })
}

fn check_col(def: &ColDef) -> Result<(), String> {
    name_ok(&def.name, "A column")?;
    type_of(def)?;
    default_of(def)?;
    if def.unsigned || def.zerofill || def.on_update.is_some() || def.srid.is_some() {
        return Err("PostgreSQL has no unsigned, zero fill, ON UPDATE or SRID attributes.".into());
    }
    if let Some(c) = &def.collation {
        collation_ok(c)?;
    }
    if matches!(auto_of(def), Auto::Identity(_)) {
        if serial_of(&def.ty).is_none() {
            return Err("Only a smallint, integer or bigint column can be an identity.".into());
        }
        if def.nullable {
            return Err("An identity column cannot allow NULL.".into());
        }
        if has_default(&def.default) {
            return Err("An identity column cannot have a default.".into());
        }
    }
    Ok(())
}

/// What follows a column's name when it is made, in ALTER TABLE .. ADD COLUMN and in CREATE TABLE alike: type, collation, then
/// the identity, the serial type or the default and NOT NULL. Checked first, so a bad definition never reaches the server.
pub(super) fn col_body(def: &ColDef) -> Result<String, String> {
    check_col(def)?;
    let ty = type_of(def)?;
    let collate = collate(&def.collation);
    Ok(match auto_of(def) {
        Auto::Serial => {
            if def.nullable || def.collation.is_some() {
                return Err("A serial column is NOT NULL and has no collation.".into());
            }
            serial_of(&def.ty).ok_or("Only a smallint, integer or bigint column can be a serial.")?.to_string()
        }
        Auto::Identity(always) => format!("{ty}{collate} GENERATED {} AS IDENTITY", if always { "ALWAYS" } else { "BY DEFAULT" }),
        Auto::No => format!("{ty}{collate}{}{}", default_of(def)?.map_or(String::new(), |d| format!(" DEFAULT {d}")), if def.nullable { "" } else { " NOT NULL" }),
    })
}

pub(super) fn comment_stmt(on: &str, text: &str) -> Result<String, String> {
    if text.contains('\0') {
        return Err("A comment cannot hold a NUL character.".into());
    }
    Ok(format!("COMMENT ON {on} IS {}", if text.is_empty() { "NULL".to_string() } else { lit(text, false) }))
}

/// Moves the identity sequence of `col` to the largest value the column holds. A column with no value of 1 or more keeps
/// the sequence's start. `table` and `col` are already quoted names, which `pg_get_serial_sequence` reads as SQL names.
fn sync_identity(table: &str, col: &str) -> String {
    let raw = col.strip_prefix('"').and_then(|c| c.strip_suffix('"')).map_or(col.to_string(), |c| c.replace("\"\"", "\""));
    format!("SELECT setval(pg_get_serial_sequence({}, {}), m) FROM (SELECT max({col}) AS m FROM {table}) s WHERE m >= 1", lit(table, false), lit(&raw, false))
}

fn key_cols(ix: &IndexDef) -> Result<String, String> {
    if ix.cols.is_empty() {
        return Err("An index needs at least one column.".into());
    }
    if ix.cols.iter().any(|c| c.prefix.is_some()) {
        return Err("PostgreSQL has no prefix lengths for index columns.".into());
    }
    for c in &ix.cols {
        name_ok(&c.name, "An index column")?;
    }
    Ok(ix.cols.iter().map(|c| format!("{}{}", q(&c.name), if c.desc { " DESC" } else { "" })).collect::<Vec<_>>().join(", "))
}

const METHODS: [&str; 6] = ["btree", "hash", "gin", "gist", "brin", "spgist"];

/// The statements that make an index: a constraint for a primary key and for one that was a unique constraint, otherwise
/// CREATE INDEX, then its comment. An index name belongs to the schema, so only the table is written after ON.
fn add_index(schema: &str, table: &str, ix: &IndexDef) -> Result<Vec<String>, String> {
    if ix.functional {
        return Err(format!("Index {} has a part this editor cannot write: an expression, a condition, included columns or an operator class.", ix.name));
    }
    if matches!(ix.kind, IndexKind::Fulltext | IndexKind::Spatial) {
        return Err("PostgreSQL has no FULLTEXT or SPATIAL index. Use a gin or gist index.".into());
    }
    if ix.invisible {
        return Err("PostgreSQL has no hidden indexes.".into());
    }
    name_ok(&ix.name, "An index")?;
    let t = D.qualified(schema, table);
    let cols = key_cols(ix)?;
    let method = ix.method.trim().to_ascii_lowercase();
    if !method.is_empty() && !METHODS.contains(&method.as_str()) {
        return Err(format!("{method:?} is not an index method."));
    }
    let mut out = Vec::new();
    if ix.kind == IndexKind::Primary || ix.constraint {
        if ix.cols.iter().any(|c| c.desc) || !(method.is_empty() || method == "btree") {
            return Err("A primary key or unique constraint is a plain btree on its columns, without sort order.".into());
        }
        let what = if ix.kind == IndexKind::Primary { "PRIMARY KEY" } else { "UNIQUE" };
        let defer = match (ix.deferrable, ix.deferred) {
            (true, true) => " DEFERRABLE INITIALLY DEFERRED",
            (true, false) => " DEFERRABLE",
            _ => "",
        };
        out.push(format!("ALTER TABLE {t} ADD CONSTRAINT {} {what} ({cols}){defer}", q(&ix.name)));
        if !ix.con_comment.is_empty() {
            out.push(comment_stmt(&format!("CONSTRAINT {} ON {t}", q(&ix.name)), &ix.con_comment)?);
        }
    } else {
        if ix.deferrable || ix.deferred {
            return Err("Only a primary key or unique constraint can be deferred.".into());
        }
        let using = if method.is_empty() { String::new() } else { format!(" USING {method}") };
        out.push(format!("CREATE {}INDEX {} ON {t}{using} ({cols})", if ix.kind == IndexKind::Unique { "UNIQUE " } else { "" }, q(&ix.name)));
    }
    if !ix.comment.is_empty() {
        out.push(comment_stmt(&format!("INDEX {}", D.qualified(schema, &ix.name)), &ix.comment)?);
    }
    Ok(out)
}

/// The statements of one change, written against the table as it is before the change (`sim`).
pub(super) fn statements(sim: &Sim, schema: &str, table: &str, c: &Change) -> Result<Vec<String>, String> {
    let t = D.qualified(schema, table);
    let alter = format!("ALTER TABLE {t}");
    Ok(match c {
        Change::Add { def, pos } => {
            no_pos(pos)?;
            let (name, body) = (q(&def.name), col_body(def)?);
            let mut v = vec![format!("{alter} ADD COLUMN {name} {body}")];
            if !def.comment.is_empty() {
                v.push(comment_stmt(&format!("COLUMN {t}.{name}"), &def.comment)?);
            }
            v
        }
        Change::Modify { from, def, pos } => {
            no_pos(pos)?;
            check_col(def)?;
            let i = sim.at(from).ok_or_else(|| format!("Column {from} is not in the table."))?;
            let old = sim.cols[i].1.as_ref().map_err(Clone::clone)?;
            let (ko, kn) = (auto_of(old), auto_of(def));
            if matches!((ko, kn), (Auto::Serial, Auto::Identity(_)) | (Auto::Identity(_), Auto::Serial)) {
                return Err(format!("Column {from} cannot change between a serial and an identity here. Drop the auto increment first."));
            }
            let (name, ty) = (q(&def.name), type_of(def)?);
            let col = format!("{alter} ALTER COLUMN {name}");
            let mut v = Vec::new();
            if *from != def.name {
                v.push(format!("{alter} RENAME COLUMN {} TO {name}", q(from)));
            }
            let type_changed = old.ty != def.ty || old.collation != def.collation;
            let identity_on = matches!(kn, Auto::Identity(_)) && !matches!(ko, Auto::Identity(_));
            // A default does not follow a new type by itself, and an identity cannot be added to a column that has one.
            let dropped = has_default(&old.default) && (type_changed || identity_on || !has_default(&def.default));
            if dropped {
                v.push(format!("{col} DROP DEFAULT"));
            }
            if type_changed {
                let collate = collate(&def.collation);
                v.push(format!("{col} TYPE {ty}{collate} USING {name}::{ty}"));
            }
            if matches!(ko, Auto::Identity(_)) && !matches!(kn, Auto::Identity(_)) {
                v.push(format!("{col} DROP IDENTITY"));
            }
            if old.nullable != def.nullable {
                v.push(format!("{col} {} NOT NULL", if def.nullable { "DROP" } else { "SET" }));
            }
            match (ko, kn) {
                (Auto::Identity(a), Auto::Identity(b)) if a != b => v.push(format!("{col} SET GENERATED {}", if b { "ALWAYS" } else { "BY DEFAULT" })),
                (_, Auto::Identity(always)) if identity_on => {
                    v.push(format!("{col} ADD GENERATED {} AS IDENTITY", if always { "ALWAYS" } else { "BY DEFAULT" }));
                    // The new sequence starts at 1 while the column keeps its values, so the next insert would repeat one.
                    v.push(sync_identity(&t, &name));
                }
                _ => {}
            }
            if let Some(d) = default_of(def)? {
                if dropped || old.default != def.default {
                    v.push(format!("{col} SET DEFAULT {d}"));
                }
            }
            if old.comment != def.comment {
                v.push(comment_stmt(&format!("COLUMN {t}.{name}"), &def.comment)?);
            }
            v
        }
        Change::Drop(n) => {
            sim.at(n).ok_or_else(|| format!("Column {n} is not in the table."))?;
            vec![format!("{alter} DROP COLUMN {}", q(n))]
        }
        Change::AddIndex(ix) => add_index(schema, table, ix)?,
        Change::DropIndex(n) => {
            let ix = sim.idx.iter().find(|x| x.name == *n).ok_or_else(|| format!("Index {n} is not on the table."))?;
            if ix.constraint { vec![format!("{alter} DROP CONSTRAINT {}", q(n))] } else { vec![format!("DROP INDEX {}", D.qualified(schema, n))] }
        }
        Change::RenameIndex { from, to, .. } => {
            name_ok(to, "An index")?;
            let ix = sim.idx.iter().find(|x| x.name == *from).ok_or_else(|| format!("Index {from} is not on the table."))?;
            if ix.constraint { vec![format!("{alter} RENAME CONSTRAINT {} TO {}", q(from), q(to))] } else { vec![format!("ALTER INDEX {} RENAME TO {}", D.qualified(schema, from), q(to))] }
        }
        Change::AddFk(k) => add_fk(sim, schema, table, k)?,
        Change::DropFk(n) => {
            let k = sim.fk_at(n).map(|i| &sim.fks[i]).ok_or_else(|| format!("Foreign key {n} is not on the table."))?;
            vec![format!("{alter} DROP CONSTRAINT {}", q(&k.name))]
        }
        Change::RenameFk { from, to } => {
            name_ok(to, "A foreign key")?;
            let k = sim.fk_at(from).map(|i| &sim.fks[i]).ok_or_else(|| format!("Foreign key {from} is not on the table."))?;
            vec![format!("{alter} RENAME CONSTRAINT {} TO {}", q(&k.name), q(to))]
        }
        Change::SetFkDeferral { name, deferrable, deferred } => {
            let k = sim.fk_at(name).map(|i| &sim.fks[i]).ok_or_else(|| format!("Foreign key {name} is not on the table."))?;
            if *deferred && !*deferrable {
                return Err("INITIALLY DEFERRED needs the key to be DEFERRABLE.".into());
            }
            let when = match (deferrable, deferred) {
                (false, _) => "NOT DEFERRABLE",
                (true, false) => "DEFERRABLE INITIALLY IMMEDIATE",
                (true, true) => "DEFERRABLE INITIALLY DEFERRED",
            };
            vec![format!("{alter} ALTER CONSTRAINT {} {when}", q(&k.name))]
        }
    })
}

/// The actions PostgreSQL has for a key. The dialog offers the four MySQL has; SET DEFAULT is here so a key that already has
/// it can be dropped and put back.
const FK_ACTIONS: [&str; 5] = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

/// The start of the statement that adds the key `name` to the table, which the run uses to tell which step is that key.
fn add_fk_head(schema: &str, table: &str, name: &str) -> String {
    format!("ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ", D.qualified(schema, table), q(name))
}

/// ADD CONSTRAINT .. FOREIGN KEY, with the match type, the actions, whether it is deferrable and NOT VALID, and the comment
/// the key has. The referenced table is always written with its schema, the key's own `ref_db` or else the table's.
fn add_fk(sim: &Sim, schema: &str, table: &str, k: &ForeignKey) -> Result<Vec<String>, String> {
    name_ok(&k.name, "A foreign key")?;
    name_ok(&k.ref_table, "The referenced table")?;
    if !k.ref_db.is_empty() {
        name_ok(&k.ref_db, "The referenced schema")?;
    }
    if k.cols.is_empty() || k.cols.len() != k.ref_cols.len() {
        return Err("Pick the same number of columns on both sides.".into());
    }
    // PostgreSQL's index key limit is 32 columns.
    if k.cols.len() > 32 {
        return Err("A foreign key can have at most 32 columns.".into());
    }
    let mut own = Vec::new();
    for c in &k.cols {
        let i = sim.at(c).ok_or_else(|| format!("Column {c} is not in the table."))?;
        own.push(q(&sim.cols[i].0));
    }
    let action = |a: &str| FK_ACTIONS.iter().find(|x| x.eq_ignore_ascii_case(a.trim())).copied().ok_or_else(|| format!("{a:?} is not a foreign key action."));
    if k.deferred && !k.deferrable {
        return Err("INITIALLY DEFERRED needs the key to be DEFERRABLE.".into());
    }
    let t = D.qualified(schema, table);
    let parent = D.qualified(if k.ref_db.is_empty() { schema } else { &k.ref_db }, &k.ref_table);
    let refs: Vec<String> = k.ref_cols.iter().map(|c| q(c)).collect();
    let mut text = format!("{}({}) REFERENCES {parent} ({})", add_fk_head(schema, table, &k.name), own.join(", "), refs.join(", "));
    if k.match_full {
        text.push_str(" MATCH FULL");
    }
    text.push_str(&format!(" ON DELETE {} ON UPDATE {}", action(&k.on_delete)?, action(&k.on_update)?));
    if k.deferrable {
        text.push_str(if k.deferred { " DEFERRABLE INITIALLY DEFERRED" } else { " DEFERRABLE" });
    }
    if k.not_valid {
        text.push_str(" NOT VALID");
    }
    let mut v = vec![text];
    if !k.comment.is_empty() {
        v.push(comment_stmt(&format!("CONSTRAINT {} ON {t}", q(&k.name)), &k.comment)?);
    }
    Ok(v)
}

/// The tables other than the changed one that the changes lock and rely on, and the keys they add that check rows. PostgreSQL
/// takes SHARE ROW EXCLUSIVE on the parent of a key it adds or whose deferral it changes and ACCESS EXCLUSIVE on the parent of
/// one it drops, a column drop that takes the table's own keys with it included. A column that changes type makes the server
/// check again every key that holds it, the table's own (on their parents) and those of other tables that point at it (on
/// those tables), all under ACCESS EXCLUSIVE. `before` is the table as it is when the first change runs.
fn others(before: &Sim, schema: &str, changes: &[Change]) -> (Vec<RefCheck>, Vec<ForeignKey>) {
    let mut sim = before.clone();
    let (mut refs, mut orphans) = (Vec::new(), Vec::new());
    let target = |k: &ForeignKey| if k.ref_db.is_empty() { schema.to_string() } else { k.ref_db.clone() };
    let dropped = |k: &ForeignKey| RefCheck { schema: target(k), table: k.ref_table.clone(), cols: Vec::new(), exclusive: true, seen: None };
    for c in changes {
        match c {
            Change::AddFk(k) => {
                refs.push(RefCheck { schema: target(k), table: k.ref_table.clone(), cols: k.ref_cols.clone(), exclusive: false, seen: Some(k.ref_seen.clone()).filter(|s| !s.is_empty()) });
                if !k.not_valid {
                    orphans.push(k.clone());
                }
            }
            Change::DropFk(n) => refs.extend(sim.fk_at(n).map(|i| &sim.fks[i]).map(dropped)),
            Change::SetFkDeferral { name, .. } => refs.extend(sim.fk_at(name).map(|i| &sim.fks[i]).map(|k| RefCheck { exclusive: false, ..dropped(k) })),
            Change::Drop(n) => refs.extend(sim.fks.iter().filter(|k| k.cols.iter().any(|c| same_name(c, n))).map(dropped)),
            Change::Modify { from, def, .. } if sim.at(from).and_then(|i| sim.cols[i].1.as_ref().ok()).is_some_and(|o| o.ty != def.ty) => {
                refs.extend(sim.fks.iter().filter(|k| k.cols.iter().any(|c| same_name(c, from))).map(dropped));
                refs.extend(sim.deps.iter().filter(|d| same_name(&d.col, from) && d.kind == "constraint" && !d.own && !d.rel_table.is_empty()).map(|d| RefCheck { schema: d.rel_schema.clone(), table: d.rel_table.clone(), cols: Vec::new(), exclusive: true, seen: None }));
            }
            _ => {}
        }
        let _ = sim.apply(c);
    }
    (refs, orphans)
}

/// Whether the changes need only SHARE ROW EXCLUSIVE on the changed table: ADD FOREIGN KEY and ALTER CONSTRAINT take no more,
/// and every other change takes ACCESS EXCLUSIVE.
fn shares_only(changes: &[Change]) -> bool {
    changes.iter().all(|c| matches!(c, Change::AddFk(_) | Change::SetFkDeferral { .. }))
}

/// The tables to lock and the mode of each, in the one order every plan takes them (by schema, then name), so two plans that
/// share tables never wait on each other in a circle. The changed table is ACCESS EXCLUSIVE unless `own_share` says the changes
/// need no more than SHARE ROW EXCLUSIVE, and another table is locked at the strongest mode any of its steps takes.
fn lock_order(schema: &str, table: &str, own_share: bool, refs: &[RefCheck]) -> Vec<(String, String, &'static str)> {
    let mut v: Vec<(String, String, bool)> = vec![(schema.to_string(), table.to_string(), !own_share)];
    for r in refs {
        match v.iter_mut().find(|e| e.0 == r.schema && e.1 == r.table) {
            Some(e) => e.2 |= r.exclusive,
            None => v.push((r.schema.clone(), r.table.clone(), r.exclusive)),
        }
    }
    v.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
    v.into_iter().map(|(s, t, ex)| (s, t, if ex { "ACCESS EXCLUSIVE" } else { "SHARE ROW EXCLUSIVE" })).collect()
}

/// A change of only the deferral of a key that is written as a drop and an add becomes ALTER CONSTRAINT, which keeps the key
/// (no rows are checked again) and takes SHARE ROW EXCLUSIVE where the drop would take ACCESS EXCLUSIVE on both tables.
fn fuse_deferral(st: &Structure, changes: &[Change]) -> Vec<Change> {
    let mut sim = Sim::of(st);
    let mut out = Vec::new();
    let mut i = 0;
    while i < changes.len() {
        if let (Change::DropFk(n), Some(Change::AddFk(new))) = (&changes[i], changes.get(i + 1)) {
            let same_but_deferral = |old: &ForeignKey| {
                old.opaque.is_empty()
                    && old.name == new.name
                    && (&old.cols, &old.ref_db, &old.ref_table, &old.ref_cols) == (&new.cols, &new.ref_db, &new.ref_table, &new.ref_cols)
                    && old.on_update.eq_ignore_ascii_case(&new.on_update)
                    && old.on_delete.eq_ignore_ascii_case(&new.on_delete)
                    && (old.match_full, old.not_valid, &old.comment) == (new.match_full, new.not_valid, &new.comment)
                    && (old.deferrable, old.deferred) != (new.deferrable, new.deferred)
                    && (new.deferrable || !new.deferred)
            };
            if sim.fk_at(n).map(|j| &sim.fks[j]).is_some_and(same_but_deferral) {
                let step = Change::SetFkDeferral { name: new.name.clone(), deferrable: new.deferrable, deferred: new.deferred };
                if sim.apply(&step).is_ok() {
                    out.push(step);
                    i += 2;
                    continue;
                }
            }
        }
        let _ = sim.apply(&changes[i]);
        out.push(changes[i].clone());
        i += 1;
    }
    out
}

/// Two names that differ only in case are two names on PostgreSQL, where the simulation compares without case.
fn case_clash(st: &Structure) -> Option<String> {
    fn clash<'a>(names: impl Iterator<Item = &'a String>) -> Option<&'a String> {
        let mut seen: Vec<String> = Vec::new();
        names.into_iter().find(|n| {
            let l = n.to_lowercase();
            let dup = seen.contains(&l);
            seen.push(l);
            dup
        })
    }
    clash(st.columns.iter().map(|c| &c.name)).map(|n| format!("column {n}")).or_else(|| clash(st.indexes.iter().map(|i| &i.name)).map(|n| format!("index {n}")))
}

/// The plan: the statements, what the undo is (or why there is none), and what the table must look like to run it.
pub(super) fn plan(table: &str, st: &Structure, changes: &[Change]) -> Result<Plan, String> {
    let schema = st.pg_schema.as_deref().ok_or("The structure has no schema.")?;
    if changes.is_empty() {
        return Err("Nothing is staged.".into());
    }
    if let Some(what) = case_clash(st) {
        return Err(format!("This table has names that differ only in case ({what}), which this editor cannot tell apart. Change it with a query."));
    }
    let changes = &fuse_deferral(st, changes);
    let mut sim = Sim::of(st);
    let mut steps = Vec::new();
    let mut types = Vec::new();
    for c in changes {
        // The lexer keeps a type text to one clause; the server then has to say it is a type, which is what stops
        // `integer REFERENCES other (id)` or `integer CHECK (..)` from adding a constraint through the type box.
        match c {
            Change::Add { def, .. } => types.push(def.ty.trim().to_string()),
            Change::Modify { from, def, .. } if sim.at(from).and_then(|i| sim.cols[i].1.as_ref().ok()).is_some_and(|o| o.ty != def.ty) => types.push(def.ty.trim().to_string()),
            _ => {}
        }
        steps.extend(statements(&sim, schema, table, c)?);
        let _ = sim.apply(c);
    }
    let (cols, idx, fks) = touched(std::slice::from_ref(&changes.to_vec()));
    // A dropped column takes whatever depends on it, and the undo only knows the ones read now, so the whole set of
    // indexes, constraints and dependents has to be the same under the lock.
    let all = changes.iter().any(|c| matches!(c, Change::Drop(_))).then(|| table_state(st));
    let (refs, orphans) = others(&Sim::of(st), schema, changes);
    let expect = Expect { snap: snapshot(st, &cols, &idx, &fks), cols, idx, fks, all, types, refs, orphans, own_share: shares_only(changes) };
    Ok(Plan { sql: super::ddl::script(&steps), steps, undo: undo_of(schema, table, st, changes), expect: Some(expect) })
}

/// Every index, constraint and dependent of the table in one comparable list.
pub(super) fn table_state(st: &Structure) -> Vec<String> {
    let mut v: Vec<String> = st.indexes.iter().map(|i| format!("index {}|{}|{}|{}|{}|{}|{}", i.name, i.def, i.comment, i.deferrable, i.deferred, i.con_comment, i.complex)).collect();
    v.extend(st.constraints.iter().map(|c| format!("constraint {}|{}|{}", c.name, c.kind, c.def)));
    v.extend(st.pg_deps.iter().map(|d| format!("depends {}|{}|{}|{}", d.col, d.kind, d.name, d.own)));
    v.extend(st.foreign_keys.iter().map(|k| format!("foreign key {}", fk_line(k))));
    v
}

/// Whether a default expression calls a function, which is the case that can be volatile. A name preceded by `::` is a
/// type, and the keywords that look like calls are not counted.
pub(super) fn calls_function(expr: &str) -> bool {
    let toks: Vec<(Tok, std::ops::Range<usize>)> = super::lex::nth_reading(expr, D, 0).into_iter().filter(|(t, _)| *t != Tok::Space).collect();
    toks.windows(2).enumerate().any(|(i, w)| {
        let name = &expr[w[0].1.clone()];
        let after_cast = i > 0 && expr[toks[i - 1].1.clone()].ends_with(':');
        w[0].0 == Tok::Word && &expr[w[1].1.clone()] == "(" && !after_cast && !matches!(name.to_ascii_lowercase().as_str(), "cast" | "coalesce" | "nullif" | "greatest" | "least" | "array" | "row" | "any" | "all" | "in" | "exists")
    })
}

fn undo_of(schema: &str, table: &str, before: &Structure, changes: &[Change]) -> Result<AlterUndo, String> {
    let mut sim = Sim::of(before);
    let mut groups = Vec::new();
    for c in changes {
        groups.push(sim.apply(c)?);
    }
    groups.reverse();
    let loss = undo_loss(&sim, &groups);
    let (cols, idx, fks) = touched(&groups);
    let mut run = sim.clone();
    let mut steps = Vec::new();
    for c in groups.iter().flatten() {
        steps.extend(statements(&run, schema, table, c)?);
        run.apply(c)?;
    }
    let (refs, orphans) = others(&sim, schema, &groups.iter().flatten().cloned().collect::<Vec<_>>());
    Ok(AlterUndo { steps, label: String::new(), loss, mode: Mode::DEFAULT, cols, idx, fks, expect: Vec::new(), refs, orphans, groups })
}

/// Why a plan did not run: the table is no longer as the plan saw it (nothing ran), a statement failed (the transaction
/// was rolled back, so nothing ran either), or the COMMIT failed, which leaves it unknown whether the server applied it.
pub(super) enum Fail {
    Changed(String),
    Run(String),
    Unknown(String),
}

/// The structure as the catalogs give it inside the transaction, which holds the table's lock by now. The search_path is
/// already the schema's, as it was for the read the plan was made from, so the two print the same text.
pub(super) async fn read_in(tx: &mut Tx<'_>, schema: &str, table: &str) -> Res<Structure> {
    let args = [Param::Text(schema.into()), Param::Text(table.into())];
    let cols = tx.fetch_bound(D.columns(), &args).await.map_err(se)?;
    let idx = tx.fetch_bound(D.indexes(), &args).await.map_err(se)?;
    let fks = tx.fetch_bound(D.foreign_keys(), &args).await.map_err(se)?;
    let cons = tx.fetch_bound(super::pg_catalog::CONSTRAINTS, &args).await.map_err(se)?;
    let deps = tx.fetch_bound(super::pg_catalog::COLUMN_DEPS, &args).await.map_err(se)?;
    Ok(Structure { columns: columns(D, &cols), indexes: indexes(&idx), foreign_keys: foreign_keys(&fks), constraints: constraints(&cons), pg_deps: col_deps(&deps), pg_schema: Some(schema.into()), ..Default::default() })
}

pub(super) async fn exec(tx: &mut Tx<'_>, db: &Db, schema: &str, sql: &str) -> Result<(), sqlx::Error> {
    let t = Instant::now();
    let res = tx.exec_bound(sql, &[]).await;
    db.note(schema, sql, t, &res);
    res.map(|_| ())
}

pub(super) fn sql_state(err: &sqlx::Error) -> Option<String> {
    err.as_database_error().and_then(|d| d.code().map(|c| c.to_string()))
}

pub(super) fn lock_error(err: &sqlx::Error, name: &str) -> Option<String> {
    // 55P03 is lock_not_available, which lock_timeout raises.
    (sql_state(err)? == "55P03").then(|| format!("The table {name} is locked by another session that did not let go within {LOCK_WAIT}. Nothing was changed."))
}

/// Whether the role may take the explicit lock the plan wants on a table. LOCK TABLE in any mode but ACCESS SHARE and ROW
/// EXCLUSIVE needs UPDATE, DELETE or TRUNCATE (MAINTAIN from PostgreSQL 17), where ADD FOREIGN KEY needs only REFERENCES on the
/// parent and DROP CONSTRAINT only ownership of the child. A role without them skips the explicit lock, and the statement
/// takes its own under the same lock_timeout. A table that is not there answers true, so the LOCK reports it gone.
pub(super) async fn may_lock(tx: &mut Tx<'_>, name: &str) -> bool {
    let sql = "SELECT COALESCE(has_table_privilege(to_regclass($1), 'UPDATE,DELETE,TRUNCATE') OR CASE WHEN current_setting('server_version_num')::int >= 170000 THEN has_table_privilege(to_regclass($1), 'MAINTAIN') ELSE false END, true)::int::bigint AS n";
    match tx.fetch_bound(sql, &[Param::Text(name.to_string())]).await {
        Ok(rows) => rows.first().and_then(|r| super::structure::num(r, "n")).is_none_or(|n| n != 0),
        Err(_) => true,
    }
}

/// The message for a key the server refused for the rows already there: how many rows have no parent and a few of them, so the
/// person sees what to fix. None when there is no such row (another session fixed them since), and the server's own error is
/// shown then. The transaction is at the savepoint before the key was added, where the rows are as the server saw them.
async fn orphan_message(tx: &mut Tx<'_>, schema: &str, table: &str, fk: &ForeignKey, undo: bool) -> Option<String> {
    let rows = tx.fetch_bound(&orphans_sql(D, schema, table, fk), &[]).await.ok()?;
    let n = rows.first().and_then(|r| super::structure::num(r, "n")).unwrap_or(0);
    if n == 0 {
        return None;
    }
    let rows = tx.fetch_bound(&orphan_sample_sql(D, schema, table, fk, ORPHAN_SAMPLE), &[]).await.unwrap_or_default();
    let shown: Vec<String> = rows.iter().map(|r| orphan_row(r, fk.cols.len())).collect();
    let parent = D.qualified(if fk.ref_db.is_empty() { schema } else { &fk.ref_db }, &fk.ref_table);
    let cols = fk.cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let refs = fk.ref_cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let (rows_word, has, it) = if n == 1 { ("row", "has", "it") } else { ("rows", "have", "them") };
    let eg = format!("{}{}", if shown.len() == 1 { "The row is " } else { "For example " }, shown.join(", "));
    let what = D.qualified(schema, table);
    // The undo cannot add the key NOT VALID, which is not how the key was before, so it asks for the rows to be fixed.
    Some(if undo {
        format!("{n} {rows_word} of {what} {has} a value in ({cols}) that {parent} ({refs}) does not hold, so the foreign key {} cannot be put back. {eg}. Fix {it}, then undo again. Nothing was changed.", fk.name)
    } else {
        format!("{n} {rows_word} of {what} {has} a value in ({cols}) that {parent} ({refs}) does not hold, so the foreign key {} cannot be added. {eg}. Fix {it} or add the key as NOT VALID. Nothing was changed.", fk.name)
    })
}

/// Runs `steps` as one transaction: the lock first, with a timeout so a busy table fails the plan instead of queueing
/// behind it, then the check that the touched columns and indexes are as `expect` says, then the statements, then COMMIT.
/// Any failure leaves the transaction to roll back.
pub(super) async fn run(db: &Db, schema: &str, table: &str, steps: &[String], expect: &Expect, read_only: bool, undo: bool) -> Result<(), Fail> {
    let mut conn = write::writer_conn(db, schema, read_only).await.map_err(Fail::Run)?;
    let mut tx = conn.begin_write(db).await.map_err(|e| Fail::Run(se(e)))?;
    let out = locked_run(&mut tx, db, schema, table, steps, expect, undo).await;
    match out {
        // The connection can drop after the server committed, so a failed COMMIT does not say that nothing changed.
        Ok(()) => tx.commit().await.map_err(|e| Fail::Unknown(format!("{}. The server did not confirm the commit, so the change may or may not have been applied. The structure is being reloaded: check it before running the change again.", se(e)))),
        Err(f) => {
            let _ = tx.rollback().await;
            Err(f)
        }
    }
}

async fn locked_run(tx: &mut Tx<'_>, db: &Db, schema: &str, table: &str, steps: &[String], expect: &Expect, undo: bool) -> Result<(), Fail> {
    exec(tx, db, schema, &format!("SET LOCAL lock_timeout = '{LOCK_WAIT}'")).await.map_err(|e| Fail::Run(se(e)))?;
    // The changed table and the other tables of its keys, in one fixed order. A key takes a lock on both of its tables, and
    // the parent can be changed by another session between the plan and here, so it is locked before it is read.
    for (s, t, mode) in lock_order(schema, table, expect.own_share, &expect.refs) {
        let name = D.qualified(&s, &t);
        if !may_lock(tx, &name).await {
            continue;
        }
        if let Err(e) = exec(tx, db, schema, &format!("LOCK TABLE {name} IN {mode} MODE")).await {
            let own = s == schema && t == table;
            return Err(match (lock_error(&e, &name), sql_state(&e).as_deref()) {
                (Some(why), _) => Fail::Run(why),
                // 42P01 is undefined_table: the referenced table was dropped after the plan was made.
                (None, Some("42P01")) if !own => Fail::Changed(format!("the referenced table {name} (it is gone)")),
                _ => Fail::Run(se(e)),
            });
        }
    }
    // Before the read and the statements alike: the plan was made from a read under this path, and the statements resolve
    // every name under it, so a type or function outside the schema is qualified in the one and found by the other.
    exec(tx, db, schema, &db.pg_set_path(schema)).await.map_err(|e| Fail::Run(se(e)))?;
    let st = read_in(tx, schema, table).await.map_err(Fail::Run)?;
    let now = snapshot(&st, &expect.cols, &expect.idx, &expect.fks);
    if now.len() != expect.snap.len() {
        return Err(Fail::Changed("the table".into()));
    }
    if let Some((what, _)) = expect.snap.iter().zip(&now).find(|(a, b)| a != b).map(|(a, _)| a) {
        return Err(Fail::Changed(what.clone()));
    }
    if let Some(all) = &expect.all {
        if table_state(&st) != *all {
            return Err(Fail::Changed("an index, constraint or other object that depends on the table".into()));
        }
    }
    // The referenced columns and unique indexes a new key leans on, read under the search_path of their own schema as the
    // plan-time read of them was.
    let mut moved = false;
    for r in expect.refs.iter().filter(|r| r.seen.is_some()) {
        let now = if r.schema == schema && r.table == table {
            ref_state(&st, &r.cols)
        } else {
            exec(tx, db, schema, &db.pg_set_path(&r.schema)).await.map_err(|e| Fail::Run(se(e)))?;
            moved = true;
            ref_state(&read_in(tx, &r.schema, &r.table).await.map_err(Fail::Run)?, &r.cols)
        };
        if Some(&now) != r.seen.as_ref() {
            return Err(Fail::Changed(format!("the referenced table {}", D.qualified(&r.schema, &r.table))));
        }
    }
    if moved {
        exec(tx, db, schema, &db.pg_set_path(schema)).await.map_err(|e| Fail::Run(se(e)))?;
    }
    for ty in &expect.types {
        let said = tx.fetch_bound("SELECT to_regtype($1)::text AS t", &[Param::Text(ty.clone())]).await;
        // PostgreSQL before 16 raises an error for text that is not a type name, later ones answer NULL.
        if said.ok().and_then(|rows| rows.first().and_then(|r| super::worker::text(r, "t"))).is_none() {
            return Err(Fail::Run(format!("{ty:?} is not a type the server knows. Write only the type, like numeric(10,2), without constraints or other column clauses. Nothing was changed.")));
        }
    }
    for (i, s) in steps.iter().enumerate() {
        // A key that checks the rows already there runs in a savepoint: the server scans the table once, and only when it
        // refuses the key are the rows that stop it looked for.
        let checks = expect.orphans.iter().find(|k| s.starts_with(&add_fk_head(schema, table, &k.name)));
        if let Some(fk) = checks {
            tx.exec_bound("SAVEPOINT quarry_fk", &[]).await.map_err(|e| Fail::Run(se(e)))?;
            if let Err(e) = exec(tx, db, schema, s).await {
                if sql_state(&e).as_deref() == Some("23503") {
                    tx.exec_bound("ROLLBACK TO SAVEPOINT quarry_fk", &[]).await.map_err(|e| Fail::Run(se(e)))?;
                    if let Some(why) = orphan_message(tx, schema, table, fk, undo).await {
                        return Err(Fail::Run(why));
                    }
                }
                return Err(Fail::Run(format!("{} (statement {} of {}). The whole plan was rolled back, so nothing was changed.", se(e), i + 1, steps.len())));
            }
            tx.exec_bound("RELEASE SAVEPOINT quarry_fk", &[]).await.map_err(|e| Fail::Run(se(e)))?;
            continue;
        }
        exec(tx, db, schema, s).await.map_err(|e| Fail::Run(format!("{} (statement {} of {}). The whole plan was rolled back, so nothing was changed.", se(e), i + 1, steps.len())))?;
    }
    Ok(())
}

/// The note for a plan that did not run.
fn fail_text(f: Fail, undo: bool) -> String {
    match f {
        Fail::Run(e) | Fail::Unknown(e) => e,
        Fail::Changed(what) if undo => format!("Undo refused: {what} changed after this change ran, so undoing it now would not put the table back."),
        Fail::Changed(what) => format!("Nothing ran: {what} changed since this plan was made. Reload the structure and stage the change again."),
    }
}

/// The undo entry for the table, which remembers the database it ran in: a PostgreSQL connection is bound to one, and
/// the undo has to run on that one's pool.
fn action(db: &Db, conn: u64, schema: &str, table: &str, u: AlterUndo) -> UndoAction {
    let mut a = undo_action(conn, schema, table, u);
    if let UndoKind::Sql { database, .. } = &mut a.kind {
        *database = db.pg_database();
    }
    a
}

/// Runs a recorded undo in one transaction, after the same check as an apply against how the change left the table.
pub(super) async fn undo(db: &Db, a: UndoAction, u: AlterUndo, read_only: bool) -> Msg {
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let expect = Expect { cols: u.cols.clone(), idx: u.idx.clone(), fks: u.fks.clone(), snap: u.expect.clone(), all: None, types: Vec::new(), refs: u.refs.clone(), orphans: u.orphans.clone(), own_share: shares_only(&u.groups.iter().flatten().cloned().collect::<Vec<_>>()) };
    let out = crate::db::sql_scope(db.conn, &schema, async { run(db, &schema, &table, &u.steps, &expect, read_only, true).await }).await;
    let unsure = matches!(out, Err(Fail::Unknown(_)));
    let res: Res<String> = out.map(|_| "Undid the structure change".to_string()).map_err(|f| fail_text(f, true));
    // The whole undo is one transaction, so a failure ran nothing and the action stays for another try, unless the commit
    // is the one that failed, when it may have run.
    let keep = (res.is_err() && !unsure).then(|| {
        let mut k = action(db, conn, &schema, &table, u);
        k.at = at;
        k
    });
    Msg::SqlAltered { conn, database: db.pg_database(), schema, table, res, undo: keep, sql_mode: None, alter: true, unsure }
}

impl Worker {
    /// Runs the plan's statements in one transaction (see `run`). `undo` reverses it when there is one; it is handed
    /// back with the answer along with how the table looked right after.
    #[allow(clippy::too_many_arguments)]
    pub fn pg_alter(&self, conn: u64, db: Db, schema: String, table: String, steps: Vec<String>, expect: Expect, undo: Option<AlterUndo>, read_only: bool, done: String) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let out = run(&db, &schema, &table, &steps, &expect, read_only, false).await;
            let unsure = matches!(out, Err(Fail::Unknown(_)));
            let res = out.map_err(|f| fail_text(f, false));
            let mut done = done;
            let undo = match (res.is_ok(), undo) {
                (true, Some(mut u)) => match load(&db, &schema, &table).await {
                    Ok(st) => {
                        u.expect = snapshot(&st, &u.cols, &u.idx, &u.fks);
                        // The undo of a dropped key puts it back and leans on the parent as it is now.
                        let mut unread = None;
                        for r in u.refs.iter_mut().filter(|r| !r.cols.is_empty()) {
                            match load(&db, &r.schema, &r.table).await {
                                Ok(p) => r.seen = Some(ref_state(&p, &r.cols)),
                                Err(err) => unread = Some(err),
                            }
                        }
                        match unread {
                            None => Some(action(&db, conn, &schema, &table, u)),
                            Some(err) => {
                                done = format!("{done}. It cannot be undone: a referenced table could not be read back ({err})");
                                None
                            }
                        }
                    }
                    Err(err) => {
                        done = format!("{done}. It cannot be undone: the table could not be read back ({err})");
                        None
                    }
                },
                _ => None,
            };
            Msg::SqlAltered { conn, database: db.pg_database(), schema, table, res: res.map(|_| done), undo, sql_mode: None, alter: true, unsure }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::structure::{ColDep, Index, IndexCol};

    #[test]
    fn expressions_stay_one_expression() {
        for ok in ["now()", "0", "(a + 1)", "'it''s'", "E'a\\'b'", "$$x; y$$", "$t$ -- not a comment $t$", "coalesce(1, 2)", "'{}'::jsonb", "now() + interval '1 day'"] {
            assert!(check_expr_pg(ok).is_ok(), "{ok}");
        }
        for bad in ["", "  ", "1; DROP TABLE t", "1 -- c", "1 /* c */", "(1", "1)", "'open", "E'a\\'", "\"open", "$$x", "1) , ADD COLUMN x int DEFAULT (2", "'a\\'; DROP TABLE t; --'"] {
            assert!(check_expr_pg(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn types_stay_one_type_name() {
        for ok in ["integer", "numeric(10,2)", "character varying(255)", "timestamp with time zone", "integer[]", "text[][]", "reporting.mood", "\"My Type\"", "public.\"My Type\"(3)"] {
            assert!(check_type_pg(ok).is_ok(), "{ok}");
        }
        for bad in ["", "integer; DROP TABLE t", "integer, ADD COLUMN x int", "integer -- x", "integer /* x */", "'x'", "integer DEFAULT 'a'", "int::text", "integer)", "numeric(10", "\"open", "integer + 1"] {
            assert!(check_type_pg(bad).is_err(), "{bad}");
        }
    }

    fn col(name: &str, ty: &str) -> Column {
        Column { name: name.into(), full_type: ty.into(), nullable: true, kind: Some(super::super::value::Kind::Text), ..Default::default() }
    }

    fn index(name: &str, def: &str, cols: &[&str]) -> Index {
        Index { name: name.into(), kind: "btree".into(), def: def.into(), cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), ..Default::default() }
    }

    fn table() -> Structure {
        let mut id = col("id", "integer");
        id.nullable = false;
        id.extra = "auto_increment GENERATED ALWAYS AS IDENTITY".into();
        let mut ix = index("t_pkey", "CREATE UNIQUE INDEX t_pkey ON public.t USING btree (id)", &["id"]);
        (ix.unique, ix.constraint, ix.primary) = (true, true, true);
        let mut by_name = index("t_name", "CREATE INDEX t_name ON public.t USING btree (name DESC, note)", &["name", "note"]);
        by_name.cols[0].desc = true;
        Structure { columns: vec![id, col("name", "character varying(40)"), col("note", "text")], indexes: vec![ix, by_name], pg_schema: Some("public".into()), ..Default::default() }
    }

    fn steps(changes: &[Change]) -> Vec<String> {
        plan("t", &table(), changes).unwrap().steps
    }

    fn def(name: &str, ty: &str) -> ColDef {
        ColDef::new(name, ty)
    }

    #[test]
    fn each_kind_of_change_has_its_statements() {
        let t = "\"public\".\"t\"";
        let mut added = def("qty", "integer");
        added.nullable = false;
        added.default = Dv::Value("0".into());
        added.comment = "How many".into();
        assert_eq!(steps(&[Change::Add { def: added, pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"qty\" integer DEFAULT '0' NOT NULL"), format!("COMMENT ON COLUMN {t}.\"qty\" IS 'How many'")]);
        assert_eq!(steps(&[Change::Drop("note".into())]), [format!("ALTER TABLE {t} DROP COLUMN \"note\"")]);
        let mut wide = def("full_name", "character varying(80)");
        wide.default = Dv::Expr("lower('X')".into());
        assert_eq!(
            steps(&[Change::Modify { from: "name".into(), def: wide, pos: Pos::Keep }]),
            [
                format!("ALTER TABLE {t} RENAME COLUMN \"name\" TO \"full_name\""),
                format!("ALTER TABLE {t} ALTER COLUMN \"full_name\" TYPE character varying(80) USING \"full_name\"::character varying(80)"),
                format!("ALTER TABLE {t} ALTER COLUMN \"full_name\" SET DEFAULT (lower('X'))"),
            ]
        );
        let mut notnull = def("note", "text");
        notnull.nullable = false;
        assert_eq!(steps(&[Change::Modify { from: "note".into(), def: notnull, pos: Pos::Keep }]), [format!("ALTER TABLE {t} ALTER COLUMN \"note\" SET NOT NULL")]);
        let ix = IndexDef { name: "t_note".into(), kind: IndexKind::Unique, cols: vec![super::super::ddl::IdxCol { name: "note".into(), prefix: None, desc: true }], comment: "by note".into(), ..Default::default() };
        assert_eq!(steps(&[Change::AddIndex(ix)]), [format!("CREATE UNIQUE INDEX \"t_note\" ON {t} (\"note\" DESC)"), "COMMENT ON INDEX \"public\".\"t_note\" IS 'by note'".to_string()]);
        assert_eq!(steps(&[Change::DropIndex("t_name".into())]), ["DROP INDEX \"public\".\"t_name\"".to_string()]);
        assert_eq!(steps(&[Change::DropIndex("t_pkey".into())]), [format!("ALTER TABLE {t} DROP CONSTRAINT \"t_pkey\"")]);
        let st = table();
        let rename = Change::RenameIndex { from: "t_name".into(), to: "t_by_name".into(), def: IndexDef::from_index(&st.indexes[1]) };
        assert_eq!(steps(&[rename]), ["ALTER INDEX \"public\".\"t_name\" RENAME TO \"t_by_name\"".to_string()]);
    }

    #[test]
    fn identity_and_serial_are_one_auto_increment() {
        let t = "\"public\".\"t\"";
        let mut auto = def("n", "bigint");
        (auto.nullable, auto.auto_increment) = (false, true);
        assert_eq!(steps(&[Change::Add { def: auto.clone(), pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"n\" bigint GENERATED BY DEFAULT AS IDENTITY")]);
        let mut serial = ColDef { serial: true, default: Dv::Expr("nextval('t_n_seq'::regclass)".into()), ..auto.clone() };
        assert_eq!(steps(&[Change::Add { def: serial.clone(), pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"n\" bigserial")]);
        // a nextval default the person typed names a sequence the column does not own: it is not written as a bigserial
        serial.serial = false;
        assert!(plan("t", &table(), &[Change::Add { def: serial, pos: Pos::Keep }]).err().unwrap_or_default().contains("identity column cannot have a default"));
        // the identity of the table's id goes away and comes back as it was
        let mut plain = def("id", "integer");
        plain.nullable = false;
        let drop = Change::Modify { from: "id".into(), def: plain, pos: Pos::Keep };
        let p = plan("t", &table(), &[drop]).unwrap();
        assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER COLUMN \"id\" DROP IDENTITY")]);
        let u = p.undo.unwrap();
        // the sequence is moved to the largest value the column holds, or the next insert repeats one
        assert_eq!(u.steps, [format!("ALTER TABLE {t} ALTER COLUMN \"id\" ADD GENERATED ALWAYS AS IDENTITY"), format!("SELECT setval(pg_get_serial_sequence('{t}', 'id'), m) FROM (SELECT max(\"id\") AS m FROM {t}) s WHERE m >= 1")]);
        assert!(u.loss.iter().any(|l| l.contains("new sequence")), "{:?}", u.loss);
        let mut text_auto = def("s", "text");
        (text_auto.nullable, text_auto.auto_increment) = (false, true);
        assert!(plan("t", &table(), &[Change::Add { def: text_auto, pos: Pos::Keep }]).is_err());
    }

    #[test]
    fn what_postgres_cannot_do_is_refused() {
        let st = table();
        let here = |c: Change| plan("t", &st, &[c]).err().unwrap_or_default();
        assert!(here(Change::Add { def: def("x", "integer"), pos: Pos::First }).contains("cannot place"));
        assert!(here(Change::Modify { from: "note".into(), def: def("note", "text"), pos: Pos::After("id".into()) }).contains("cannot place"));
        let mut unsigned = def("x", "integer");
        unsigned.unsigned = true;
        assert!(here(Change::Add { def: unsigned, pos: Pos::Keep }).contains("unsigned"));
        assert!(here(Change::AddFk(Default::default())).contains("needs a name"));
        let spatial = IndexDef { name: "g".into(), kind: IndexKind::Fulltext, cols: vec![super::super::ddl::IdxCol { name: "note".into(), prefix: None, desc: false }], ..Default::default() };
        assert!(here(Change::AddIndex(spatial)).contains("FULLTEXT"));
        let mut hostile = def("x", "integer; DROP TABLE t");
        assert!(here(Change::Add { def: hostile.clone(), pos: Pos::Keep }).contains("type"));
        hostile.ty = "integer".into();
        hostile.default = Dv::Expr("1); DROP TABLE t; --".into());
        assert!(here(Change::Add { def: hostile, pos: Pos::Keep }).contains("default expression"));
        let long = def(&"x".repeat(64), "integer");
        assert!(here(Change::Add { def: long, pos: Pos::Keep }).contains("63 bytes"));
    }

    #[test]
    fn dropping_a_column_takes_its_indexes_and_undo_brings_them_back_whole() {
        let p = plan("t", &table(), &[Change::Drop("name".into())]).unwrap();
        let u = p.undo.unwrap();
        // the column returns last, then the index it was in, with both its columns
        assert_eq!(u.steps, ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"name\" character varying(40)", "CREATE INDEX \"t_name\" ON \"public\".\"t\" (\"name\" DESC, \"note\")"]);
    }

    #[test]
    fn a_primary_key_undo_gives_back_nullability() {
        let ix = IndexDef { name: "t_note_pkey".into(), kind: IndexKind::Primary, cols: vec![super::super::ddl::IdxCol { name: "note".into(), prefix: None, desc: false }], ..Default::default() };
        let mut st = table();
        st.indexes.remove(0);
        let p = plan("t", &st, &[Change::AddIndex(ix)]).unwrap();
        assert_eq!(p.steps, ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_note_pkey\" PRIMARY KEY (\"note\")"]);
        assert_eq!(p.undo.unwrap().steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_note_pkey\"", "ALTER TABLE \"public\".\"t\" ALTER COLUMN \"note\" DROP NOT NULL"]);
    }

    #[test]
    fn indexes_the_editor_cannot_write_back_are_flagged() {
        assert!(!IndexDef::from_index(&table().indexes[1]).functional);
        // the catalog says which indexes have a part the columns do not
        let complex = Index { complex: true, ..index("i", "CREATE INDEX i ON public.t USING btree (name) WHERE (note IS NOT NULL)", &["name"]) };
        assert!(IndexDef::from_index(&complex).functional);
        // a quoted name that holds the words of the text is no longer read as the text
        let tricky = index("a USING (b) WHERE", "CREATE INDEX \"a USING (b) WHERE\" ON public.t USING btree (name)", &["name"]);
        assert!(!IndexDef::from_index(&tricky).functional);
        let gin = Index { kind: "GIN".into(), ..index("i", "CREATE INDEX i ON public.t USING gin (name)", &["name"]) };
        assert_eq!(IndexDef::from_index(&gin).method, "gin");
    }

    #[test]
    fn a_constraint_is_added_back_deferrable_with_its_comment() {
        let ix = IndexDef { name: "t_u".into(), kind: IndexKind::Unique, constraint: true, deferrable: true, deferred: true, con_comment: "one per name".into(), cols: vec![super::super::ddl::IdxCol { name: "name".into(), prefix: None, desc: false }], ..Default::default() };
        assert_eq!(steps(&[Change::AddIndex(ix.clone())]), ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_u\" UNIQUE (\"name\") DEFERRABLE INITIALLY DEFERRED".to_string(), "COMMENT ON CONSTRAINT \"t_u\" ON \"public\".\"t\" IS 'one per name'".to_string()]);
        assert_eq!(steps(&[Change::AddIndex(IndexDef { deferred: false, con_comment: String::new(), ..ix.clone() })]), ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_u\" UNIQUE (\"name\") DEFERRABLE".to_string()]);
        assert!(plan("t", &table(), &[Change::AddIndex(IndexDef { constraint: false, ..ix })]).is_err(), "a plain index cannot be deferred");
    }

    #[test]
    fn a_digit_led_word_with_a_dollar_is_refused() {
        for bad in ["1$a$", "x + 1$a$ y", "2$"] {
            assert!(check_expr_pg(bad).is_err(), "{bad}");
        }
        for ok in ["a$b", "now()", "$$1$$"] {
            assert!(check_expr_pg(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn a_collation_outside_pg_catalog_is_written_with_its_schema() {
        let mut d = def("s", "text");
        d.collation = Some("\"Other\".\"my \"\"coll\"\"\"".into());
        assert_eq!(steps(&[Change::Add { def: d.clone(), pos: Pos::Keep }]), ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"s\" text COLLATE \"Other\".\"my \"\"coll\"\"\"".to_string()]);
        d.collation = Some("en_US.utf8".into());
        assert_eq!(steps(&[Change::Add { def: d, pos: Pos::Keep }]), ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"s\" text COLLATE \"en_US.utf8\"".to_string()]);
    }

    #[test]
    fn function_calls_in_a_default_are_found() {
        for yes in ["random()", "gen_random_uuid()", "now() + interval '1 day'", "lower(upper('x'))", "1 + abs(-2)"] {
            assert!(calls_function(yes), "{yes}");
        }
        for no in ["0", "'x'::numeric(10,2)", "(1 + 2)", "'{}'::jsonb", "CAST('1' AS integer)", "coalesce(1, 2)", "ARRAY[1, 2]"] {
            assert!(!calls_function(no), "{no}");
        }
    }

    #[test]
    fn a_function_default_on_a_table_with_rows_is_warned_about() {
        let mut st = table();
        let with = |d: Dv| vec![Change::Add { def: ColDef { default: d, ..def("tok", "uuid") }, pos: Pos::Keep }];
        assert!(super::super::ddl::warnings(&st, &with(Dv::Expr("gen_random_uuid()".into()))).is_empty(), "no rows known, nothing to rewrite");
        st.status.rows = Some(3);
        let w = super::super::ddl::warnings(&st, &with(Dv::Expr("gen_random_uuid()".into())));
        assert!(w.iter().any(|x| x.contains("once for each of the table's rows") && x.contains("exclusive lock")), "{w:?}");
        assert!(super::super::ddl::warnings(&st, &with(Dv::Expr("'x'::text".into()))).is_empty());
        assert!(super::super::ddl::warnings(&st, &with(Dv::Value("x".into()))).is_empty());
    }

    #[test]
    fn a_dropped_column_comes_back_without_what_it_cannot_hold() {
        let mut st = table();
        // a NOT NULL column without a default and its primary key
        st.columns[2].nullable = false;
        let mut pk = index("t_note_pkey", "CREATE UNIQUE INDEX t_note_pkey ON public.t USING btree (note)", &["note"]);
        (pk.unique, pk.constraint, pk.primary) = (true, true, true);
        st.indexes.push(pk);
        let u = plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap();
        assert_eq!(u.steps, ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"note\" text".to_string(), "CREATE INDEX \"t_name\" ON \"public\".\"t\" (\"name\" DESC, \"note\")".to_string()]);
        assert!(u.loss.iter().any(|l| l.contains("allowing NULL") && l.contains("primary key")), "{:?}", u.loss);
        // with a default the column keeps NOT NULL
        st.columns[2].default = Some("x".into());
        let u = plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap();
        assert!(u.steps[0].ends_with("DEFAULT 'x' NOT NULL"), "{:?}", u.steps);
        assert!(u.steps.iter().any(|s| s.contains("PRIMARY KEY")));
    }

    #[test]
    fn what_the_server_drops_with_a_column_must_be_something_the_undo_restores() {
        let dep = |kind: &str, name: &str| ColDep { col: "note".into(), kind: kind.into(), name: name.into(), what: format!("{kind} {name}"), own: true, ..Default::default() };
        let undo = |deps: Vec<ColDep>| {
            let mut st = table();
            st.pg_deps = deps;
            plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo
        };
        assert!(undo(vec![dep("index", "t_name"), dep("default", "")]).is_ok());
        for bad in [dep("constraint", "t_note_check"), dep("index", "t_partial"), dep("other", "trigger x")] {
            let why = undo(vec![dep("index", "t_name"), bad.clone()]).unwrap_err();
            assert!(why.contains(&bad.what), "{why}");
        }
        // the plan itself is not refused, and it is checked again under the lock
        let mut st = table();
        st.pg_deps = vec![dep("constraint", "t_note_check")];
        let p = plan("t", &st, &[Change::Drop("note".into())]).unwrap();
        assert!(p.expect.unwrap().all.is_some());
    }
    fn fk(name: &str, cols: &[&str], ref_db: &str, table: &str, ref_cols: &[&str]) -> ForeignKey {
        let own = |v: &[&str]| v.iter().map(|c| (*c).to_string()).collect::<Vec<_>>();
        ForeignKey { name: name.into(), db: "public".into(), table: "t".into(), cols: own(cols), ref_db: ref_db.into(), ref_table: table.into(), ref_cols: own(ref_cols), on_update: "NO ACTION".into(), on_delete: "NO ACTION".into(), ..Default::default() }
    }

    #[test]
    fn a_foreign_key_is_written_with_everything_it_carries() {
        let t = "\"public\".\"t\"";
        let plain = fk("t_note_fk", &["note"], "", "p", &["code"]);
        assert_eq!(steps(&[Change::AddFk(plain.clone())]), [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_note_fk\" FOREIGN KEY (\"note\") REFERENCES \"public\".\"p\" (\"code\") ON DELETE NO ACTION ON UPDATE NO ACTION")]);
        let full = ForeignKey { match_full: true, deferrable: true, deferred: true, not_valid: true, comment: "it's".into(), on_delete: "set null".into(), on_update: "Cascade".into(), ..fk("t_pair_fk", &["name", "note"], "other", "p", &["a", "b"]) };
        assert_eq!(
            steps(&[Change::AddFk(full)]),
            [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_pair_fk\" FOREIGN KEY (\"name\", \"note\") REFERENCES \"other\".\"p\" (\"a\", \"b\") MATCH FULL ON DELETE SET NULL ON UPDATE CASCADE DEFERRABLE INITIALLY DEFERRED NOT VALID"), format!("COMMENT ON CONSTRAINT \"t_pair_fk\" ON {t} IS 'it''s'")]
        );
        let deferrable = ForeignKey { deferrable: true, on_delete: "SET DEFAULT".into(), ..plain.clone() };
        assert!(steps(&[Change::AddFk(deferrable)])[0].ends_with("ON DELETE SET DEFAULT ON UPDATE NO ACTION DEFERRABLE"));
        // a key on a column the table does not have, a bad action, mismatched sides and a name that is too long are refused
        for bad in [fk("k", &["nope"], "", "p", &["a"]), ForeignKey { on_delete: "CASCADE; DROP TABLE t".into(), ..plain.clone() }, fk("k", &["note", "name"], "", "p", &["a"]), fk(&"n".repeat(64), &["note"], "", "p", &["a"]), ForeignKey { deferred: true, ..plain.clone() }] {
            assert!(plan("t", &table(), &[Change::AddFk(bad.clone())]).is_err(), "{bad:?}");
        }
        // a name that needs quoting is quoted, and a key is dropped and renamed as a constraint
        let mut st = table();
        st.foreign_keys = vec![fk("Odd \"name\"", &["note"], "", "p", &["code"])];
        let go = |c: Change| plan("t", &st, &[c]).unwrap().steps;
        assert_eq!(go(Change::DropFk("Odd \"name\"".into())), [format!("ALTER TABLE {t} DROP CONSTRAINT \"Odd \"\"name\"\"\"")]);
        assert_eq!(go(Change::RenameFk { from: "Odd \"name\"".into(), to: "fresh".into() }), [format!("ALTER TABLE {t} RENAME CONSTRAINT \"Odd \"\"name\"\"\" TO \"fresh\"")]);
        assert!(plan("t", &st, &[Change::RenameFk { from: "Odd \"name\"".into(), to: "x".repeat(64) }]).is_err());
        assert!(plan("t", &st, &[Change::DropFk("missing".into())]).is_err());
    }

    #[test]
    fn the_undo_of_a_key_change_is_the_key_as_it_was() {
        let mut st = table();
        let old = ForeignKey { match_full: true, deferrable: true, comment: "c".into(), on_delete: "CASCADE".into(), ..fk("t_note_fk", &["note"], "other", "p", &["code"]) };
        st.foreign_keys = vec![old.clone()];
        let undo = plan("t", &st, &[Change::DropFk("t_note_fk".into())]).unwrap().undo.unwrap();
        assert_eq!(undo.groups, [vec![Change::AddFk(old.clone())]]);
        assert!(undo.steps[0].contains("MATCH FULL ON DELETE CASCADE ON UPDATE NO ACTION DEFERRABLE") && undo.steps.len() == 2, "{:?}", undo.steps);
        // the undo of a rename renames back, and a key written back from the catalog is dropped again by name
        let undo = plan("t", &st, &[Change::RenameFk { from: "t_note_fk".into(), to: "fresh".into() }]).unwrap().undo.unwrap();
        assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" RENAME CONSTRAINT \"fresh\" TO \"t_note_fk\""]);
        let undo = plan("t", &table(), &[Change::AddFk(old)]).unwrap().undo.unwrap();
        assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_note_fk\""]);
        // what one of the key's own columns can drop: the key, which is put back after the column
        let mut st = table();
        st.foreign_keys = vec![fk("t_note_fk", &["note"], "", "p", &["code"])];
        st.pg_deps = vec![ColDep { col: "note".into(), kind: "constraint".into(), name: "t_note_fk".into(), what: "constraint t_note_fk on table t".into(), own: true, ..Default::default() }];
        let undo = plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap();
        // the column first, then the index that held it, then the key
        assert!(undo.steps.len() == 3 && undo.steps[0].contains("ADD COLUMN \"note\"") && undo.steps[1].starts_with("CREATE INDEX") && undo.steps[2].contains("ADD CONSTRAINT \"t_note_fk\""), "{:?}", undo.steps);
        // the same name on a key of another table is not this table's, and the undo says why it has none
        st.pg_deps[0].own = false;
        assert!(plan("t", &st, &[Change::Drop("note".into())]).unwrap().undo.unwrap_err().contains("another table"));
    }

    #[test]
    fn a_plan_locks_every_table_of_its_keys_in_one_order() {
        let r = |s: &str, t: &str, exclusive: bool| RefCheck { schema: s.into(), table: t.into(), cols: Vec::new(), exclusive, seen: None };
        // whatever order the changes name the tables in, the locks go by schema and name, the changed table among them
        let a = lock_order("public", "orders", false, &[r("public", "customers", false), r("archive", "old", false)]);
        let b = lock_order("public", "orders", false, &[r("archive", "old", false), r("public", "customers", false)]);
        assert_eq!(a, b);
        let names: Vec<(&str, &str)> = a.iter().map(|(s, t, _)| (s.as_str(), t.as_str())).collect();
        assert_eq!(names, [("archive", "old"), ("public", "customers"), ("public", "orders")]);
        // the changed table is always exclusive, a parent only where a key is dropped, and a table asked twice is locked once
        assert_eq!(a.iter().map(|x| x.2).collect::<Vec<_>>(), ["SHARE ROW EXCLUSIVE", "SHARE ROW EXCLUSIVE", "ACCESS EXCLUSIVE"]);
        let c = lock_order("public", "orders", false, &[r("public", "customers", false), r("public", "customers", true), r("public", "orders", false)]);
        assert_eq!(c.iter().map(|x| (x.1.as_str(), x.2)).collect::<Vec<_>>(), [("customers", "ACCESS EXCLUSIVE"), ("orders", "ACCESS EXCLUSIVE")]);
    }

    #[test]
    fn a_plan_that_only_adds_keys_locks_each_table_share_row_exclusive() {
        let k = fk("t_note_fk", &["note"], "", "p", &["code"]);
        let p = plan("t", &table(), &[Change::AddFk(k.clone())]).unwrap();
        let e = p.expect.unwrap();
        assert!(e.own_share, "ADD FOREIGN KEY needs no more than SHARE ROW EXCLUSIVE on the child");
        assert_eq!(lock_order("public", "t", e.own_share, &e.refs).iter().map(|x| x.2).collect::<Vec<_>>(), ["SHARE ROW EXCLUSIVE", "SHARE ROW EXCLUSIVE"]);
        // any other change of the table brings ACCESS EXCLUSIVE back, and so does a drop of a key
        let p = plan("t", &table(), &[Change::AddFk(k.clone()), Change::Drop("name".into())]).unwrap();
        assert!(!p.expect.unwrap().own_share);
        let mut st = table();
        st.foreign_keys = vec![k];
        assert!(!plan("t", &st, &[Change::DropFk("t_note_fk".into())]).unwrap().expect.unwrap().own_share);
        // the undo of an add is a drop, which takes the stronger lock on both tables
        let p = plan("t", &table(), &[Change::AddFk(fk("t_note_fk", &["note"], "", "p", &["code"]))]).unwrap();
        assert!(!shares_only(&p.undo.unwrap().groups.concat()));
    }

    #[test]
    fn a_change_of_only_when_a_key_is_checked_is_alter_constraint_and_so_is_its_undo() {
        let t = "\"public\".\"t\"";
        let mut st = table();
        let old = ForeignKey { deferrable: true, comment: "c".into(), ..fk("t_note_fk", &["note"], "public", "p", &["code"]) };
        st.foreign_keys = vec![old.clone()];
        let edit = |new: ForeignKey| vec![Change::DropFk("t_note_fk".into()), Change::AddFk(new)];
        // deferrable and initially immediate becomes initially deferred: no drop, no new check of the rows
        let p = plan("t", &st, &edit(ForeignKey { deferred: true, ..old.clone() })).unwrap();
        assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER CONSTRAINT \"t_note_fk\" DEFERRABLE INITIALLY DEFERRED")]);
        let e = p.expect.as_ref().unwrap();
        assert!(e.own_share && e.orphans.is_empty() && e.refs.iter().all(|r| !r.exclusive), "{e:?}");
        assert_eq!(p.undo.unwrap().steps, [format!("ALTER TABLE {t} ALTER CONSTRAINT \"t_note_fk\" DEFERRABLE INITIALLY IMMEDIATE")]);
        let p = plan("t", &st, &edit(ForeignKey { deferrable: false, ..old.clone() })).unwrap();
        assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER CONSTRAINT \"t_note_fk\" NOT DEFERRABLE")]);
        // anything else that changes is still a drop and an add
        let p = plan("t", &st, &edit(ForeignKey { deferred: true, on_delete: "CASCADE".into(), ..old.clone() })).unwrap();
        assert_eq!(p.steps.len(), 3, "{:?}", p.steps);
        assert!(plan("t", &st, &edit(ForeignKey { deferred: true, deferrable: false, ..old })).is_err());
        assert!(plan("t", &st, &[Change::SetFkDeferral { name: "t_note_fk".into(), deferrable: false, deferred: true }]).is_err());
    }

    #[test]
    fn a_key_whose_name_differs_only_in_case_is_found_by_its_exact_name() {
        let t = "\"public\".\"t\"";
        let mut st = table();
        st.foreign_keys = vec![fk("T_fk", &["note"], "", "p", &["code"]), fk("t_fk", &["name"], "", "p", &["code"])];
        let go = |c: Change| plan("t", &st, &[c]).unwrap();
        // each name finds its own key, in the statement and in the locks and the undo
        assert_eq!(go(Change::DropFk("t_fk".into())).steps, [format!("ALTER TABLE {t} DROP CONSTRAINT \"t_fk\"")]);
        assert_eq!(go(Change::DropFk("T_fk".into())).steps, [format!("ALTER TABLE {t} DROP CONSTRAINT \"T_fk\"")]);
        assert_eq!(go(Change::RenameFk { from: "t_fk".into(), to: "other".into() }).steps, [format!("ALTER TABLE {t} RENAME CONSTRAINT \"t_fk\" TO \"other\"")]);
        let undo = go(Change::DropFk("t_fk".into())).undo.unwrap();
        assert!(undo.steps[0].contains("FOREIGN KEY (\"name\")"), "{:?}", undo.steps);
        // a name that is not there is not matched by another case
        assert!(plan("t", &st, &[Change::DropFk("T_FK".into())]).is_err());
        // the snapshot the transaction compares reads the right key for each
        let snap = snapshot(&st, &[], &[], &["t_fk".to_string(), "T_fk".to_string()]);
        assert!(snap[0].1.as_deref().unwrap().contains("|name|") && snap[1].1.as_deref().unwrap().contains("|note|"), "{snap:?}");
        // a key of one case can be added beside one of another
        let two = plan("t", &st, &[Change::AddFk(fk("T_FK", &["id"], "", "p", &["id"]))]).unwrap();
        assert!(two.steps[0].contains("\"T_FK\""));
    }

    #[test]
    fn a_column_that_changes_type_locks_the_tables_of_the_keys_on_it() {
        let mut st = table();
        st.foreign_keys = vec![fk("t_note_fk", &["note"], "other", "p", &["code"])];
        st.pg_deps = vec![
            ColDep { col: "note".into(), kind: "constraint".into(), name: "t_note_fk".into(), what: "constraint t_note_fk on table t".into(), own: true, ..Default::default() },
            ColDep { col: "id".into(), kind: "constraint".into(), name: "c_tid_fk".into(), what: "constraint c_tid_fk on table c".into(), own: false, rel_schema: "kids".into(), rel_table: "c".into() },
        ];
        let locks = |col: &str, ty: &str| {
            let mut d = def(col, ty);
            d.nullable = col != "id";
            let e = plan("t", &st, &[Change::Modify { from: col.into(), def: d, pos: Pos::Keep }]).unwrap().expect.unwrap();
            lock_order("public", "t", e.own_share, &e.refs).into_iter().map(|(s, t, m)| format!("{s}.{t} {m}")).collect::<Vec<_>>()
        };
        // retyping a column of an own key takes the parent, retyping a referenced column takes the child of the other table
        assert_eq!(locks("note", "character varying(9)"), ["other.p ACCESS EXCLUSIVE", "public.t ACCESS EXCLUSIVE"]);
        assert_eq!(locks("id", "bigint"), ["kids.c ACCESS EXCLUSIVE", "public.t ACCESS EXCLUSIVE"]);
        // a column no key holds takes nothing else, and a change that keeps the type neither
        assert_eq!(locks("name", "text"), ["public.t ACCESS EXCLUSIVE"]);
        assert_eq!(locks("note", "text"), ["public.t ACCESS EXCLUSIVE"]);
    }

    #[test]
    fn a_refused_column_drop_leaves_the_simulated_table_as_it_was() {
        let mut st = table();
        let mut opaque = fk("t_note_fk", &["note"], "", "p", &["code"]);
        opaque.opaque = "ON DELETE SET NULL (note)".into();
        st.foreign_keys = vec![opaque, fk("t_name_fk", &["name"], "", "p", &["code"])];
        let mut sim = Sim::of(&st);
        assert!(sim.apply(&Change::Drop("note".into())).unwrap_err().contains("cannot put back"));
        assert_eq!((sim.cols.len(), sim.fks.len(), sim.idx.len()), (3, 2, 2), "a refused change changed the simulation");
        // the plan still runs the drop, and the steps after it were written against the table as it was
        let p = plan("t", &st, &[Change::Drop("note".into()), Change::DropFk("t_name_fk".into())]).unwrap();
        assert_eq!(p.steps.len(), 2);
        assert!(p.undo.is_err());
    }
}
