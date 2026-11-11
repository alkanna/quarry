//! The PostgreSQL text of a structure change. The staged `Change`s and their undo groups are the ones `ddl` builds for
//! MySQL; what differs is the statements. PostgreSQL has no one statement that alters a column whole, so a change is
//! written as the clauses that differ from the column as the simulation holds it (type with USING, nullability, default,
//! identity, comment), and an index is its own CREATE INDEX or constraint. Every name is quoted and qualified with the
//! table's schema, so the search_path never decides which table is meant. The two pieces of user text, a type and a
//! default expression, are checked with the PostgreSQL lexer to stay one clause.
//!
//! DDL is transactional here, which MySQL's is not: a plan and its undo each run as one transaction that takes the
//! table's lock, reads the touched columns and indexes again and refuses to run when another session changed them.

use super::ddl::{snapshot, touched, undo_action, undo_loss, AlterUndo, Change, ColDef, Dv, Expect, IndexDef, IndexKind, Mode, Plan, Pos, Sim};
use super::exec::Tx;
use super::lex::{every_reading, Tok};
use super::structure::{columns, constraints, foreign_keys, indexes, load, Column, Index, Structure};
use super::value::{lit, Param};
use super::{se, write, Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use std::time::Instant;

const D: Dialect = Dialect::Postgres;

/// How long the plan waits for the table's lock. A session that holds it longer is named in the error, and nothing ran.
const LOCK_WAIT: &str = "3s";

/// PostgreSQL cuts a longer name without a word, so the staged name would not be the one the server keeps.
const MAX_NAME: usize = 63;

fn q(name: &str) -> String {
    D.quote_ident(name)
}

fn name_ok(name: &str, what: &str) -> Result<(), String> {
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
    Type,
}

/// The text must stay one clause in every reading the server may give it: no `;`, no comment that could swallow the rest
/// of the statement, balanced brackets and quotes. Both readings of backslashes are tried, so a string that one of them
/// ends early cannot hide a statement in the other.
fn scan(s: &str, what: Text) -> Result<(), String> {
    let name = if what == Text::Expr { "default expression" } else { "type" };
    let one = format!("The {name} must be {}, without ; or comments.", if what == Text::Expr { "one expression" } else { "a type name like numeric(10,2)" });
    if s.trim().is_empty() {
        return Err(format!("The {name} is empty."));
    }
    for (n, toks) in every_reading(s, D).iter().enumerate() {
        let mut depth = 0i32;
        for (tok, r) in toks {
            let t = &s[r.clone()];
            match tok {
                Tok::Comment | Tok::ExecOpen | Tok::ExecClose => return Err(one),
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
/// owns; an identity is the standard form. Both read as auto increment, and the serial keeps its default as an expression
/// so changing it back puts the same default on.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Auto {
    No,
    Serial,
    Identity(bool),
}

fn auto_of(c: &ColDef) -> Auto {
    match (&c.default, c.auto_increment) {
        (_, false) => Auto::No,
        (Dv::Expr(x), true) if x.trim_start().to_ascii_lowercase().starts_with("nextval(") => Auto::Serial,
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
    Ok(ColDef { default, auto_increment: c.extra.contains("auto_increment"), identity_always: identity && c.extra.contains("GENERATED ALWAYS AS IDENTITY"), collation: c.collation.clone(), nullable: c.nullable, comment: c.comment.clone(), ..ColDef::new(&c.name, &c.full_type) })
}

/// The index parts of a `pg_get_indexdef` text: the access method and the key parts, with what follows the closing
/// bracket (a WHERE, INCLUDE, WITH or TABLESPACE clause).
fn index_text(def: &str) -> Option<(&str, Vec<&str>, &str)> {
    let rest = &def[def.find(" USING ")? + 7..];
    let open = rest.find('(')?;
    let method = rest[..open].trim();
    let (mut depth, mut quoted, mut start, mut parts) = (0, false, open + 1, Vec::new());
    for (i, c) in rest.char_indices().skip_while(|(i, _)| *i < open) {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    parts.push(rest[start..i].trim());
                    return Some((method, parts, rest[i + 1..].trim()));
                }
            }
            ',' if !quoted && depth == 1 => {
                parts.push(rest[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// The name a key part spells: bare, or quoted with doubled quotes.
fn part_name(p: &str) -> String {
    match p.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => p.to_string(),
    }
}

/// Whether an index is only columns, each ascending or descending: what `IndexDef` can write back. A partial index,
/// included columns, an operator class, a collation, NULLS ordering or storage options are in the text after the columns
/// and would be lost by dropping the index and adding it again.
pub(super) fn index_is_plain(ix: &Index) -> bool {
    let Some((_, parts, tail)) = index_text(&ix.def) else { return false };
    tail.is_empty() && parts.len() == ix.cols.len() && parts.iter().zip(&ix.cols).all(|(p, c)| {
        let p = p.strip_suffix(" DESC").unwrap_or(p);
        !c.name.is_empty() && part_name(p) == c.name
    })
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
/// the server reads it as the column's type, which is also what it reports back.
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
        name_ok(c, "A collation")?;
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

fn comment_stmt(on: &str, text: &str) -> Result<String, String> {
    if text.contains('\0') {
        return Err("A comment cannot hold a NUL character.".into());
    }
    Ok(format!("COMMENT ON {on} IS {}", if text.is_empty() { "NULL".to_string() } else { lit(text, false) }))
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
        out.push(format!("ALTER TABLE {t} ADD CONSTRAINT {} {what} ({cols})", q(&ix.name)));
    } else {
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
            check_col(def)?;
            let (name, ty) = (q(&def.name), type_of(def)?);
            let collate = def.collation.as_ref().map_or(String::new(), |c| format!(" COLLATE {}", q(c)));
            let body = match auto_of(def) {
                Auto::Serial => {
                    if def.nullable || def.collation.is_some() {
                        return Err("A serial column is NOT NULL and has no collation.".into());
                    }
                    serial_of(&def.ty).ok_or("Only a smallint, integer or bigint column can be a serial.")?.to_string()
                }
                Auto::Identity(always) => format!("{ty}{collate} GENERATED {} AS IDENTITY", if always { "ALWAYS" } else { "BY DEFAULT" }),
                Auto::No => format!("{ty}{collate}{}{}", default_of(def)?.map_or(String::new(), |d| format!(" DEFAULT {d}")), if def.nullable { "" } else { " NOT NULL" }),
            };
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
                let collate = def.collation.as_ref().map_or(String::new(), |c| format!(" COLLATE {}", q(c)));
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
                (_, Auto::Identity(always)) if identity_on => v.push(format!("{col} ADD GENERATED {} AS IDENTITY", if always { "ALWAYS" } else { "BY DEFAULT" })),
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
        Change::AddFk(_) | Change::DropFk(_) => return Err("Foreign keys of a PostgreSQL table are not edited here yet.".into()),
    })
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
    let mut sim = Sim::of(st);
    let mut steps = Vec::new();
    for c in changes {
        steps.extend(statements(&sim, schema, table, c)?);
        let _ = sim.apply(c);
    }
    let (cols, idx, fks) = touched(std::slice::from_ref(&changes.to_vec()));
    let expect = Expect { snap: snapshot(st, &cols, &idx, &fks), cols, idx, fks };
    Ok(Plan { sql: super::ddl::script(&steps), steps, undo: undo_of(schema, table, st, changes), expect: Some(expect) })
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
    Ok(AlterUndo { steps, label: String::new(), loss, mode: Mode::DEFAULT, cols, idx, fks, expect: Vec::new(), groups })
}

/// Why a plan did not run: the table is no longer as the plan saw it (nothing ran), or a statement failed (the
/// transaction was rolled back, so nothing ran either).
pub(super) enum Fail {
    Changed(String),
    Run(String),
}

/// The structure as the catalogs give it inside the transaction, which holds the table's lock by now.
async fn read_in(tx: &mut Tx<'_>, schema: &str, table: &str) -> Res<Structure> {
    let args = [Param::Text(schema.into()), Param::Text(table.into())];
    let cols = tx.fetch_bound(D.columns(), &args).await.map_err(se)?;
    let idx = tx.fetch_bound(D.indexes(), &args).await.map_err(se)?;
    let fks = tx.fetch_bound(D.foreign_keys(), &args).await.map_err(se)?;
    let cons = tx.fetch_bound(super::pg_catalog::CONSTRAINTS, &args).await.map_err(se)?;
    Ok(Structure { columns: columns(D, &cols), indexes: indexes(&idx), foreign_keys: foreign_keys(&fks), constraints: constraints(&cons), pg_schema: Some(schema.into()), ..Default::default() })
}

async fn exec(tx: &mut Tx<'_>, db: &Db, schema: &str, sql: &str) -> Result<(), sqlx::Error> {
    let t = Instant::now();
    let res = tx.exec_bound(sql, &[]).await;
    db.note(schema, sql, t, &res);
    res.map(|_| ())
}

fn lock_error(err: &sqlx::Error) -> Option<String> {
    let code = err.as_database_error().and_then(|d| d.code().map(|c| c.to_string()))?;
    // 55P03 is lock_not_available, which lock_timeout raises.
    (code == "55P03").then(|| format!("The table is locked by another session that did not let go within {LOCK_WAIT}. Nothing was changed."))
}

/// Runs `steps` as one transaction: the lock first, with a timeout so a busy table fails the plan instead of queueing
/// behind it, then the check that the touched columns and indexes are as `expect` says, then the statements, then COMMIT.
/// Any failure leaves the transaction to roll back.
pub(super) async fn run(db: &Db, schema: &str, table: &str, steps: &[String], expect: &Expect, read_only: bool) -> Result<(), Fail> {
    let mut conn = write::writer_conn(db, schema, read_only).await.map_err(Fail::Run)?;
    let mut tx = conn.begin_write().await.map_err(|e| Fail::Run(se(e)))?;
    let out = locked_run(&mut tx, db, schema, table, steps, expect).await;
    match out {
        Ok(()) => tx.commit().await.map_err(|e| Fail::Run(format!("{}. Nothing was changed.", se(e)))),
        Err(f) => {
            let _ = tx.rollback().await;
            Err(f)
        }
    }
}

async fn locked_run(tx: &mut Tx<'_>, db: &Db, schema: &str, table: &str, steps: &[String], expect: &Expect) -> Result<(), Fail> {
    let t = D.qualified(schema, table);
    exec(tx, db, schema, &format!("SET LOCAL lock_timeout = '{LOCK_WAIT}'")).await.map_err(|e| Fail::Run(se(e)))?;
    if let Err(e) = exec(tx, db, schema, &format!("LOCK TABLE {t} IN ACCESS EXCLUSIVE MODE")).await {
        return Err(Fail::Run(lock_error(&e).unwrap_or_else(|| se(e))));
    }
    let st = read_in(tx, schema, table).await.map_err(Fail::Run)?;
    let now = snapshot(&st, &expect.cols, &expect.idx, &expect.fks);
    if now.len() != expect.snap.len() {
        return Err(Fail::Changed("the table".into()));
    }
    if let Some((what, _)) = expect.snap.iter().zip(&now).find(|(a, b)| a != b).map(|(a, _)| a) {
        return Err(Fail::Changed(what.clone()));
    }
    // After the read, so the types it compares are written as the pool's sessions write them. User text such as a type
    // name then resolves in the table's own schema.
    exec(tx, db, schema, &format!("SET LOCAL search_path = {}", q(schema))).await.map_err(|e| Fail::Run(se(e)))?;
    for (i, s) in steps.iter().enumerate() {
        exec(tx, db, schema, s).await.map_err(|e| Fail::Run(format!("{} (statement {} of {}). The whole plan was rolled back, so nothing was changed.", se(e), i + 1, steps.len())))?;
    }
    Ok(())
}

/// The note for a plan that did not run.
fn fail_text(f: Fail, undo: bool) -> String {
    match f {
        Fail::Run(e) => e,
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
    let expect = Expect { cols: u.cols.clone(), idx: u.idx.clone(), fks: u.fks.clone(), snap: u.expect.clone() };
    let res: Res<String> = crate::db::sql_scope(db.conn, &schema, async { run(db, &schema, &table, &u.steps, &expect, read_only).await.map(|_| "Undid the structure change".to_string()).map_err(|f| fail_text(f, true)) }).await;
    // The whole undo is one transaction, so a failure ran nothing and the action stays for another try.
    let keep = res.is_err().then(|| {
        let mut k = action(db, conn, &schema, &table, u);
        k.at = at;
        k
    });
    Msg::SqlAltered { conn, schema, table, res, undo: keep, sql_mode: None, alter: true }
}

impl Worker {
    /// Runs the plan's statements in one transaction (see `run`). `undo` reverses it when there is one; it is handed
    /// back with the answer along with how the table looked right after.
    #[allow(clippy::too_many_arguments)]
    pub fn pg_alter(&self, conn: u64, db: Db, schema: String, table: String, steps: Vec<String>, expect: Expect, undo: Option<AlterUndo>, read_only: bool, done: String) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let res = run(&db, &schema, &table, &steps, &expect, read_only).await.map_err(|f| fail_text(f, false));
            let mut done = done;
            let undo = match (res.is_ok(), undo) {
                (true, Some(mut u)) => match load(&db, &schema, &table).await {
                    Ok(st) => {
                        u.expect = snapshot(&st, &u.cols, &u.idx, &u.fks);
                        Some(action(&db, conn, &schema, &table, u))
                    }
                    Err(err) => {
                        done = format!("{done}. It cannot be undone: the table could not be read back ({err})");
                        None
                    }
                },
                _ => None,
            };
            Msg::SqlAltered { conn, schema, table, res: res.map(|_| done), undo, sql_mode: None, alter: true }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::structure::IndexCol;

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
        let mut serial = auto.clone();
        serial.default = Dv::Expr("nextval('t_n_seq'::regclass)".into());
        assert_eq!(steps(&[Change::Add { def: serial, pos: Pos::Keep }]), [format!("ALTER TABLE {t} ADD COLUMN \"n\" bigserial")]);
        // the identity of the table's id goes away and comes back as it was
        let mut plain = def("id", "integer");
        plain.nullable = false;
        let drop = Change::Modify { from: "id".into(), def: plain, pos: Pos::Keep };
        let p = plan("t", &table(), &[drop]).unwrap();
        assert_eq!(p.steps, [format!("ALTER TABLE {t} ALTER COLUMN \"id\" DROP IDENTITY")]);
        assert_eq!(p.undo.unwrap().steps, [format!("ALTER TABLE {t} ALTER COLUMN \"id\" ADD GENERATED ALWAYS AS IDENTITY")]);
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
        assert!(here(Change::AddFk(Default::default())).contains("Foreign keys"));
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
        assert!(IndexDef::from_index(&table().indexes[1]).functional == false);
        for def in [
            "CREATE INDEX i ON public.t USING btree (name) WHERE (note IS NOT NULL)",
            "CREATE INDEX i ON public.t USING btree (name) INCLUDE (note)",
            "CREATE INDEX i ON public.t USING btree (name text_pattern_ops)",
            "CREATE INDEX i ON public.t USING btree (name NULLS FIRST)",
            "CREATE INDEX i ON public.t USING btree (name) WITH (fillfactor='70')",
        ] {
            assert!(IndexDef::from_index(&index("i", def, &["name"])).functional, "{def}");
        }
        let gin = |def: &str, col: &str| Index { kind: "GIN".into(), ..index("i", def, &[col]) };
        assert!(!IndexDef::from_index(&gin("CREATE INDEX i ON public.t USING gin (\"Name\")", "Name")).functional);
        assert_eq!(IndexDef::from_index(&gin("CREATE INDEX i ON public.t USING gin (name)", "name")).method, "gin");
    }
}
