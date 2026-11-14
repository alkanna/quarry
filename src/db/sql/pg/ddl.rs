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

use crate::db::sql::ddl::{fk_line, orphan_row, orphan_sample_sql, orphans_sql, ref_state, same_name, snapshot, touched, undo_action, undo_loss, AlterUndo, Change, ColDef, Dv, Expect, IndexDef, IndexKind, Mode, Plan, Pos, RefCheck, Sim, ORPHAN_SAMPLE};
use crate::db::sql::exec::{bound_pg, sql_state, Conn, Tx};
use crate::db::sql::lex::{every_reading, Tok};
use crate::db::sql::structure::{col_deps, columns, constraints, foreign_keys, indexes, load, pg_checks, CheckConstraint, Column, ForeignKey, Structure};
use crate::db::sql::value::{lit, pg_lit, Param};
use crate::db::sql::write::PgIds;
use crate::db::sql::pg::LOCK_WAIT_SECS;
use crate::db::sql::{se, write, Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use std::time::Instant;

const D: Dialect = Dialect::Postgres;

/// PostgreSQL cuts a longer name without a word, so the staged name would not be the one the server keeps.
const MAX_NAME: usize = 63;

pub(in crate::db::sql) fn q(name: &str) -> String {
    D.quote_ident(name)
}

pub(in crate::db::sql) fn qn(schema: &str, name: &str) -> String {
    D.qualified(schema, name)
}

pub(in crate::db::sql) fn name_ok(name: &str, what: &str) -> Result<(), String> {
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
    /// A CHECK constraint's expression.
    Check,
    /// The WHERE condition of a partial index.
    Pred,
    Type,
}

/// The text must stay one clause in every reading the server may give it: no `;`, no comment that could swallow the rest
/// of the statement, balanced brackets and quotes. Both readings of backslashes are tried, so a string that one of them
/// ends early cannot hide a statement in the other.
fn scan(s: &str, what: Text) -> Result<(), String> {
    let name = match what {
        Text::Expr => "default expression",
        Text::Cond => "WHEN condition",
        Text::Check => "CHECK expression",
        Text::Pred => "index condition",
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
pub(in crate::db::sql) fn check_expr_pg(s: &str) -> Result<(), String> {
    scan(s, Text::Expr)
}

/// A trigger's WHEN condition typed by the person: one expression, like a default. See `scan`.
pub fn check_cond_pg(s: &str) -> Result<(), String> {
    scan(s, Text::Cond)
}

/// A CHECK expression typed by the person: one expression, like a WHEN condition, and no call the read-only check refuses,
/// because the server runs it on every row written later. See `scan`.
pub fn check_constraint_pg(s: &str) -> Result<(), String> {
    scan(s, Text::Check)?;
    match crate::db::sql::lex::refused_call(s) {
        Some(f) => Err(format!("The CHECK expression calls {f}, which acts outside the row and is refused.")),
        None => Ok(()),
    }
}

/// The WHERE condition of a partial index typed by the person: one expression, and no call the read-only check refuses, because
/// the server runs it for every row written later. See `scan`.
pub(in crate::db::sql) fn check_predicate_pg(s: &str) -> Result<(), String> {
    scan(s, Text::Pred)?;
    match crate::db::sql::lex::refused_call(s) {
        Some(f) => Err(format!("The index condition calls {f}, which acts outside the row and is refused.")),
        None => Ok(()),
    }
}

/// A column type typed by the person must stay one type name. See `scan`.
pub(in crate::db::sql) fn check_type_pg(s: &str) -> Result<(), String> {
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
pub(in crate::db::sql) fn col_def(c: &Column) -> Result<ColDef, String> {
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
pub fn identity_type(ty: &str) -> bool {
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
pub(in crate::db::sql) fn col_body(def: &ColDef) -> Result<String, String> {
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

pub(in crate::db::sql) fn comment_stmt(on: &str, text: &str) -> Result<String, String> {
    if text.contains('\0') {
        return Err("A comment cannot hold a NUL character.".into());
    }
    Ok(format!("COMMENT ON {on} IS {}", if text.is_empty() { "NULL".to_string() } else { pg_lit(text) }))
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
        // Only an undo gets here with an index the fields cannot describe: the server's own text puts back exactly what it had.
        // A constraint's index is not made by CREATE INDEX, so those stay out.
        if ix.raw.starts_with("CREATE ") && !ix.constraint && ix.kind != IndexKind::Primary {
            name_ok(&ix.name, "An index")?;
            let mut out = vec![ix.raw.clone()];
            if !ix.comment.is_empty() {
                out.push(comment_stmt(&format!("INDEX {}", D.qualified(schema, &ix.name)), &ix.comment)?);
            }
            return Ok(out);
        }
        return Err(format!("Index {} has a part this editor cannot write: an expression, an operator class, a collation, a sort of NULLs, storage options or a tablespace.", ix.name));
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
    let btree = method.is_empty() || method == "btree";
    let predicate = ix.predicate.trim();
    let include = if ix.include.is_empty() {
        String::new()
    } else {
        if !matches!(method.as_str(), "" | "btree" | "gist" | "spgist") {
            return Err(format!("A {method} index cannot have INCLUDE columns. Only btree, gist and spgist can."));
        }
        for c in &ix.include {
            name_ok(c, "An included column")?;
        }
        format!(" INCLUDE ({})", ix.include.iter().map(|c| q(c)).collect::<Vec<_>>().join(", "))
    };
    let mut out = Vec::new();
    if ix.kind == IndexKind::Primary || ix.constraint {
        if ix.cols.iter().any(|c| c.desc) || !btree {
            return Err("A primary key or unique constraint is a plain btree on its columns, without sort order.".into());
        }
        if !predicate.is_empty() || ix.concurrently {
            return Err("A primary key or unique constraint cannot be partial or built CONCURRENTLY. Add a unique index for that.".into());
        }
        let what = if ix.kind == IndexKind::Primary { "PRIMARY KEY" } else { "UNIQUE" };
        let defer = match (ix.deferrable, ix.deferred) {
            (true, true) => " DEFERRABLE INITIALLY DEFERRED",
            (true, false) => " DEFERRABLE",
            _ => "",
        };
        out.push(format!("ALTER TABLE {t} ADD CONSTRAINT {} {what} ({cols}){include}{defer}", q(&ix.name)));
        if !ix.con_comment.is_empty() {
            out.push(comment_stmt(&format!("CONSTRAINT {} ON {t}", q(&ix.name)), &ix.con_comment)?);
        }
    } else {
        if ix.deferrable || ix.deferred {
            return Err("Only a primary key or unique constraint can be deferred.".into());
        }
        if ix.kind == IndexKind::Unique && !btree {
            return Err("Only a btree index can be UNIQUE.".into());
        }
        if !btree && ix.cols.iter().any(|c| c.desc) {
            return Err("Only a btree index has a sort order.".into());
        }
        let using = if method.is_empty() { String::new() } else { format!(" USING {method}") };
        if !predicate.is_empty() {
            check_predicate_pg(predicate)?;
        }
        let wher = if predicate.is_empty() { String::new() } else { format!(" WHERE {predicate}") };
        out.push(format!("CREATE {}INDEX {}{} ON {t}{using} ({cols}){include}{wher}", if ix.kind == IndexKind::Unique { "UNIQUE " } else { "" }, if ix.concurrently { "CONCURRENTLY " } else { "" }, q(&ix.name)));
    }
    if !ix.comment.is_empty() {
        out.push(comment_stmt(&format!("INDEX {}", D.qualified(schema, &ix.name)), &ix.comment)?);
    }
    Ok(out)
}

/// The quoted name of the index a CREATE INDEX CONCURRENTLY step makes.
fn concurrent_name(step: &str) -> Option<&str> {
    let rest = step.strip_prefix("CREATE INDEX CONCURRENTLY ").or_else(|| step.strip_prefix("CREATE UNIQUE INDEX CONCURRENTLY "))?;
    let b = rest.as_bytes();
    if b.first() != Some(&b'"') {
        return None;
    }
    let mut i = 1;
    while i < b.len() {
        if b[i] == b'"' {
            if b.get(i + 1) == Some(&b'"') {
                i += 2;
                continue;
            }
            return Some(&rest[..=i]);
        }
        i += 1;
    }
    None
}

/// The steps that run in the plan's transaction, and the ones that cannot: CREATE INDEX CONCURRENTLY is refused inside a
/// transaction block, and so is the comment on the index it makes, which has to wait for the index.
pub(in crate::db::sql) fn split_steps(steps: &[String]) -> (Vec<&String>, Vec<&String>) {
    let (mut now, mut later) = (Vec::new(), Vec::new());
    let mut names: Vec<&str> = Vec::new();
    for s in steps {
        if let Some(n) = concurrent_name(s) {
            names.push(n);
            later.push(s);
        } else if s.starts_with("COMMENT ON INDEX ") && names.iter().any(|n| s.contains(&format!(".{n} IS "))) {
            later.push(s);
        } else {
            now.push(s);
        }
    }
    (now, later)
}

/// One statement through the extended protocol, so the server takes exactly one statement from the text.
async fn exec_one(conn: &mut Conn, sql: &str) -> Result<(), sqlx::Error> {
    match conn {
        Conn::Pg(c) => bound_pg(sql, &[]).execute(&mut **c).await.map(|_| ()),
        Conn::My(_) => Err(sqlx::Error::Protocol("not a PostgreSQL connection".into())),
    }
}

/// Runs the steps that cannot share the plan's transaction, one at a time on a connection of their own, after it committed.
/// A build that fails leaves an INVALID index behind, and only such an index of this table is dropped again, so a retry does
/// not meet its name and an index of another table with the same name is never touched. The error says the rest was applied.
async fn run_concurrent(db: &Db, schema: &str, table: &str, steps: &[String]) -> Result<(), String> {
    let n = steps.len();
    let late = |what: String| format!("{what}. The other changes were already applied, and the index was not built. Reload the structure and stage it again.");
    let mut conn = write::writer_conn(db, schema, false).await.map_err(&late)?;
    // The session is read-only and has a statement timeout, and this statement runs outside the transaction that lifts both.
    // The connection is closed after, so none of it reaches the next user of a connection.
    let prep = [db.pg_set_path(schema), "SET default_transaction_read_only = off".to_string(), "SET statement_timeout = 0".to_string()];
    let mut res = Ok(());
    for p in &prep {
        if let Err(e) = exec_one(&mut conn, p).await {
            res = Err(late(se(e)));
            break;
        }
    }
    if res.is_ok() {
        for (i, s) in steps.iter().enumerate() {
            let t = Instant::now();
            let out = exec_one(&mut conn, s).await;
            db.note(schema, s, t, &out);
            if let Err(e) = out {
                if let Some(name) = concurrent_name(s) {
                    let full = format!("{}.{name}", q(schema));
                    let find = "SELECT 1 AS x FROM pg_index WHERE indexrelid = to_regclass($1) AND indrelid = to_regclass($2) AND NOT indisvalid";
                    let params = [Param::Text(full.clone()), Param::Text(D.qualified(schema, table))];
                    let invalid = match &mut conn {
                        Conn::Pg(c) => bound_pg(find, &params).fetch_all(&mut **c).await.is_ok_and(|r| !r.is_empty()),
                        Conn::My(_) => false,
                    };
                    if invalid {
                        let _ = exec_one(&mut conn, &format!("DROP INDEX {full}")).await;
                    }
                }
                res = Err(late(format!("{} (concurrent statement {} of {})", se(e), i + 1, n)));
                break;
            }
        }
    }
    let _ = conn.close().await;
    res
}

/// The statements of one change, written against the table as it is before the change (`sim`).
pub(in crate::db::sql) fn statements(sim: &Sim, schema: &str, table: &str, c: &Change) -> Result<Vec<String>, String> {
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
            if let Some(d) = default_of(def)? && (dropped || old.default != def.default) {
                v.push(format!("{col} SET DEFAULT {d}"));
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
        Change::AddCheck(k) => vec![add_check(schema, table, k)?],
        Change::DropCheck(n) => {
            let k = sim.check_at(n).map(|i| &sim.checks[i]).ok_or_else(|| format!("Check constraint {n} is not on the table."))?;
            vec![format!("{alter} DROP CONSTRAINT {}", q(&k.name))]
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

/// The start of the statement that adds the CHECK `name`, which the run uses to tell which steps add one.
fn add_check_head(schema: &str, table: &str, name: &str) -> String {
    format!("ALTER TABLE {} ADD CONSTRAINT {} CHECK ", D.qualified(schema, table), q(name))
}

/// ADD CONSTRAINT .. CHECK. NOT VALID skips the rows already there, and NOT ENFORCED (PostgreSQL 18) is how a constraint that
/// was read that way is written back; the server takes only one of the two.
fn add_check(schema: &str, table: &str, k: &CheckConstraint) -> Result<String, String> {
    name_ok(&k.name, "A check constraint")?;
    check_constraint_pg(&k.expr)?;
    let mut text = format!("{}({})", add_check_head(schema, table, &k.name), k.expr.trim());
    if k.no_inherit {
        text.push_str(" NO INHERIT");
    }
    if k.not_enforced {
        text.push_str(" NOT ENFORCED");
    } else if k.not_valid {
        text.push_str(" NOT VALID");
    }
    Ok(text)
}

/// PostgreSQL runs a CHECK on every row written and assumes it is IMMUTABLE, so one that reaches a volatile or stable function (which
/// could write, call out or read other tables) is refused. The create dialog does the same for the checks of a new table.
async fn added_checks_immutable(tx: &mut Tx<'_>, schema: &str, table: &str, added: &[String]) -> Result<(), Fail> {
    let Some(oid) = super::create::relation_oid(tx, &D.qualified(schema, table)).await.map_err(Fail::Run)? else { return Ok(()) };
    let rows = tx.fetch_bound(super::create::CHECK_VOLATILE, &[Param::Text(oid)]).await.map_err(|e| Fail::Run(se(e)))?;
    for r in &rows {
        let con = crate::db::sql::worker::text(r, "con").unwrap_or_default();
        if !added.contains(&con) {
            continue;
        }
        let f = crate::db::sql::worker::text(r, "fn").unwrap_or_default();
        let vol = if crate::db::sql::worker::text(r, "vol").as_deref() == Some("s") { "STABLE" } else { "VOLATILE" };
        return Err(Fail::Run(format!("The CHECK {con} calls {f}, which is {vol}. PostgreSQL runs a CHECK on every row written and assumes it is IMMUTABLE, so only IMMUTABLE functions are allowed there. Nothing was changed.")));
    }
    Ok(())
}

/// The actions PostgreSQL has for a key. The dialog offers the four MySQL has; SET DEFAULT is here so a key that already has
/// it can be dropped and put back.
const FK_ACTIONS: [&str; 5] = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

/// The start of the statement that adds the key `name` to the table, which the run uses to tell which step is that key.
fn add_fk_head(schema: &str, table: &str, name: &str) -> String {
    format!("ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ", D.qualified(schema, table), q(name))
}

/// What follows a key's own columns: the parent, the match type, the actions and whether it is deferrable. The parent is always
/// written with its schema, the key's own `ref_db` or else `schema`. ADD CONSTRAINT and CREATE TABLE write the same text.
pub(in crate::db::sql) fn fk_refs(schema: &str, k: &ForeignKey) -> Result<String, String> {
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
    let action = |a: &str| FK_ACTIONS.iter().find(|x| x.eq_ignore_ascii_case(a.trim())).copied().ok_or_else(|| format!("{a:?} is not a foreign key action."));
    if k.deferred && !k.deferrable {
        return Err("INITIALLY DEFERRED needs the key to be DEFERRABLE.".into());
    }
    let parent = D.qualified(if k.ref_db.is_empty() { schema } else { &k.ref_db }, &k.ref_table);
    let refs: Vec<String> = k.ref_cols.iter().map(|c| q(c)).collect();
    let mut text = format!("REFERENCES {parent} ({})", refs.join(", "));
    if k.match_full {
        text.push_str(" MATCH FULL");
    }
    text.push_str(&format!(" ON DELETE {} ON UPDATE {}", action(&k.on_delete)?, action(&k.on_update)?));
    if k.deferrable {
        text.push_str(if k.deferred { " DEFERRABLE INITIALLY DEFERRED" } else { " DEFERRABLE" });
    }
    Ok(text)
}

/// ADD CONSTRAINT .. FOREIGN KEY with `fk_refs`, NOT VALID, and the comment the key has.
fn add_fk(sim: &Sim, schema: &str, table: &str, k: &ForeignKey) -> Result<Vec<String>, String> {
    name_ok(&k.name, "A foreign key")?;
    let refs = fk_refs(schema, k)?;
    let mut own = Vec::new();
    for c in &k.cols {
        let i = sim.at(c).ok_or_else(|| format!("Column {c} is not in the table."))?;
        own.push(q(&sim.cols[i].0));
    }
    let t = D.qualified(schema, table);
    let mut text = format!("{}({}) {refs}", add_fk_head(schema, table, &k.name), own.join(", "));
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
pub(in crate::db::sql) fn plan(table: &str, st: &Structure, changes: &[Change]) -> Result<Plan, String> {
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
    let (cols, idx, fks, checks) = touched(std::slice::from_ref(&changes.to_vec()));
    // A dropped column takes whatever depends on it, and the undo only knows the ones read now, so the whole set of
    // indexes, constraints and dependents has to be the same under the lock.
    let all = changes.iter().any(|c| matches!(c, Change::Drop(_))).then(|| table_state(st));
    let (refs, orphans) = others(&Sim::of(st), schema, changes);
    let expect = Expect { snap: snapshot(st, &cols, &idx, &fks, &checks), cols, idx, fks, checks, all, types, refs, orphans, own_share: shares_only(changes) };
    Ok(Plan { steps, undo: undo_of(schema, table, st, changes), expect: Some(expect) })
}

/// Every index, constraint and dependent of the table in one comparable list.
pub(in crate::db::sql) fn table_state(st: &Structure) -> Vec<String> {
    let mut v: Vec<String> = st.indexes.iter().map(|i| format!("index {}|{}|{}|{}|{}|{}|{}", i.name, i.def, i.comment, i.deferrable, i.deferred, i.con_comment, i.complex)).collect();
    v.extend(st.constraints.iter().map(|c| format!("constraint {}|{}|{}", c.name, c.kind, c.def)));
    v.extend(st.pg_deps.iter().map(|d| format!("depends {}|{}|{}|{}", d.col, d.kind, d.name, d.own)));
    v.extend(st.foreign_keys.iter().map(|k| format!("foreign key {}", fk_line(k))));
    v
}

/// Whether a default expression calls a function, which is the case that can be volatile. A name preceded by `::` is a
/// type, and the keywords that look like calls are not counted.
pub(in crate::db::sql) fn calls_function(expr: &str) -> bool {
    let toks: Vec<(Tok, std::ops::Range<usize>)> = crate::db::sql::lex::nth_reading(expr, D, 0).into_iter().filter(|(t, _)| *t != Tok::Space).collect();
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
    let (cols, idx, fks, checks) = touched(&groups);
    let mut run = sim.clone();
    let mut steps = Vec::new();
    for c in groups.iter().flatten() {
        steps.extend(statements(&run, schema, table, c)?);
        run.apply(c)?;
    }
    let (refs, orphans) = others(&sim, schema, &groups.iter().flatten().cloned().collect::<Vec<_>>());
    Ok(AlterUndo { steps, label: String::new(), loss, mode: Mode::DEFAULT, cols, idx, fks, checks, expect: Vec::new(), refs, orphans, groups })
}

/// Why a plan did not run: the table is no longer as the plan saw it (nothing ran), a statement failed (the transaction
/// was rolled back, so nothing ran either), or the COMMIT failed, which leaves it unknown whether the server applied it.
pub(in crate::db::sql) enum Fail {
    Changed(String),
    Run(String),
    Unknown(String),
}

/// The structure as the catalogs give it inside the transaction, which holds the table's lock by now. The search_path is
/// already the schema's, as it was for the read the plan was made from, so the two print the same text.
pub(in crate::db::sql) async fn read_in(tx: &mut Tx<'_>, schema: &str, table: &str) -> Res<Structure> {
    let args = [Param::Text(schema.into()), Param::Text(table.into())];
    let cols = tx.fetch_bound(D.columns(), &args).await.map_err(se)?;
    let idx = tx.fetch_bound(D.indexes(), &args).await.map_err(se)?;
    let fks = tx.fetch_bound(D.foreign_keys(), &args).await.map_err(se)?;
    let cons = tx.fetch_bound(crate::db::sql::pg::catalog::CONSTRAINTS, &args).await.map_err(se)?;
    let deps = tx.fetch_bound(crate::db::sql::pg::catalog::COLUMN_DEPS, &args).await.map_err(se)?;
    let constraints = constraints(&cons);
    Ok(Structure { columns: columns(D, &cols), indexes: indexes(&idx), foreign_keys: foreign_keys(&fks), checks: pg_checks(&constraints), constraints, pg_deps: col_deps(&deps), pg_schema: Some(schema.into()), ..Default::default() })
}

pub(in crate::db::sql) async fn exec(tx: &mut Tx<'_>, db: &Db, schema: &str, sql: &str) -> Result<(), sqlx::Error> {
    let t = Instant::now();
    let res = tx.exec_bound(sql, &[]).await;
    db.note(schema, sql, t, &res);
    res.map(|_| ())
}

/// A `SET LOCAL` through `exec`, so the status log shows it, and it ends with the transaction.
pub(in crate::db::sql) async fn set_local(tx: &mut Tx<'_>, db: &Db, schema: &str, name: &str, value: &str) -> Result<(), sqlx::Error> {
    exec(tx, db, schema, &format!("SET LOCAL {name} = '{}'", value.replace('\'', "''"))).await
}

/// The search_path the plan was made under, for the read and for every statement after it, so a type or function outside the
/// schema is qualified in the one and found by the other.
pub(in crate::db::sql) async fn set_path(tx: &mut Tx<'_>, db: &Db, schema: &str) -> Result<(), sqlx::Error> {
    exec(tx, db, schema, &db.pg_set_path(schema)).await
}

/// The lock timeout of a structure change, a trigger edit or a create: a session that holds the lock longer is named in the
/// error, and nothing ran.
pub(in crate::db::sql) async fn set_lock_wait(tx: &mut Tx<'_>, db: &Db, schema: &str) -> Result<(), sqlx::Error> {
    set_local(tx, db, schema, "lock_timeout", &format!("{LOCK_WAIT_SECS}s")).await
}

/// The error for a COMMIT that failed. The connection can drop after the server committed, so it does not say that nothing
/// changed. `next` is what to do about it, which depends on the screen the change came from.
pub(in crate::db::sql) fn commit_unknown(err: sqlx::Error, next: &str) -> String {
    format!("{}. The server did not confirm the commit, so the change may or may not have been applied. {next}", se(err))
}

pub(in crate::db::sql) const RELOAD_STRUCTURE: &str = "The structure is being reloaded: check it before running the change again.";

pub(in crate::db::sql) fn lock_error(err: &sqlx::Error, name: &str) -> Option<String> {
    // 55P03 is lock_not_available, which lock_timeout raises.
    (sql_state(err)? == "55P03").then(|| crate::db::errors::lock_held(name, LOCK_WAIT_SECS))
}

/// For each of `tables` (a name and whether only a plain table may be locked): whether the role may take the explicit lock the plan
/// wants. LOCK TABLE in any mode but ACCESS SHARE and ROW EXCLUSIVE needs UPDATE, DELETE or TRUNCATE (MAINTAIN from PostgreSQL 17),
/// where ADD FOREIGN KEY needs only REFERENCES on the parent and DROP CONSTRAINT only ownership of the child. A role without them
/// skips the explicit lock, and the statement takes its own under the same lock_timeout. A table that is not there answers true, so
/// the LOCK reports it gone. `plain_only` is for a trigger edit: a view's lock would reach the tables under it and a foreign table
/// cannot be locked, so anything but a plain table answers false. One query answers for all of them.
pub(in crate::db::sql) async fn may_lock(tx: &mut Tx<'_>, tables: &[(&str, bool)]) -> Vec<bool> {
    if tables.is_empty() {
        return Vec::new();
    }
    let values = (1..=tables.len()).map(|i| format!("(${i}::text, {i})")).collect::<Vec<_>>().join(", ");
    let sql = format!("SELECT t.ord::bigint AS ord, (SELECT c.relkind::text FROM pg_class c WHERE c.oid = to_regclass(t.n)) AS k, COALESCE(has_table_privilege(to_regclass(t.n), 'UPDATE,DELETE,TRUNCATE') OR CASE WHEN current_setting('server_version_num')::int >= 170000 THEN has_table_privilege(to_regclass(t.n), 'MAINTAIN') ELSE false END, true)::int::bigint AS n FROM (VALUES {values}) AS t(n, ord)");
    let params: Vec<Param> = tables.iter().map(|(n, _)| Param::Text(n.to_string())).collect();
    let mut out = vec![true; tables.len()];
    if let Ok(rows) = tx.fetch_bound(&sql, &params).await {
        for r in &rows {
            let Some(i) = crate::db::sql::structure::num(r, "ord").and_then(|o| usize::try_from(o - 1).ok()).filter(|i| *i < tables.len()) else { continue };
            let plain = crate::db::sql::worker::text(r, "k").is_none_or(|k| k == "r");
            out[i] = if tables[i].1 && !plain { false } else { crate::db::sql::structure::num(r, "n").is_none_or(|n| n != 0) };
        }
    }
    out
}

/// One table of `may_lock`'s question.
pub(in crate::db::sql) async fn may_lock_one(tx: &mut Tx<'_>, name: &str) -> bool {
    may_lock(tx, &[(name, false)]).await[0]
}

/// A table a transaction locks before it reads or changes anything.
pub(in crate::db::sql) struct Lock {
    pub name: String,
    pub mode: &'static str,
    /// Only a plain table is locked (`may_lock`).
    pub plain_only: bool,
    /// What a `Fail::Changed` says when the table is gone (42P01) after the plan was made, or None when that is an error as any other.
    pub gone: Option<String>,
}

/// Runs `body` in one write transaction that holds `locks` first, in the order given (the caller fixes one order for every plan, so
/// two that share tables never wait on each other in a circle), with the lock timeout of `set_lock_wait` so a busy table fails the
/// plan instead of queueing behind it. A lock the role may not take is skipped (`may_lock`). The commit is the last thing: a
/// failure of the body rolls back, and a failed COMMIT is `Fail::Unknown`, with `next` saying what to do about it.
pub(in crate::db::sql) async fn run_locked<T>(db: &Db, scope: &str, read_only: bool, locks: &[Lock], next: &str, body: impl AsyncFnOnce(&mut Tx<'_>) -> Result<T, Fail>) -> Result<T, Fail> {
    let mut conn = write::writer_conn(db, scope, read_only).await.map_err(Fail::Run)?;
    let mut tx = conn.begin_write(db).await.map_err(|e| Fail::Run(se(e)))?;
    let out = async {
        set_lock_wait(&mut tx, db, scope).await.map_err(|e| Fail::Run(se(e)))?;
        let names: Vec<(&str, bool)> = locks.iter().map(|l| (l.name.as_str(), l.plain_only)).collect();
        let may = may_lock(&mut tx, &names).await;
        for (l, may) in locks.iter().zip(may) {
            if !may {
                continue;
            }
            if let Err(e) = exec(&mut tx, db, scope, &format!("LOCK TABLE {} IN {} MODE", l.name, l.mode)).await {
                return Err(match (lock_error(&e, &l.name), sql_state(&e).as_deref(), &l.gone) {
                    (Some(why), _, _) => Fail::Run(why),
                    // 42P01 is undefined_table: the table was dropped after the plan was made.
                    (None, Some("42P01"), Some(what)) => Fail::Changed(what.clone()),
                    _ => Fail::Run(se(e)),
                });
            }
        }
        body(&mut tx).await
    }
    .await;
    match out {
        Ok(v) => tx.commit().await.map(|_| v).map_err(|e| Fail::Unknown(commit_unknown(e, next))),
        Err(f) => {
            let _ = tx.rollback().await;
            Err(f)
        }
    }
}

/// The note for a plan that did not run. `changed` words a refusal because the objects were not as expected, which reads
/// differently for each feature and for an undo.
pub(in crate::db::sql) fn fail_text(f: Fail, changed: impl FnOnce(&str) -> String) -> String {
    match f {
        Fail::Run(e) | Fail::Unknown(e) => e,
        Fail::Changed(what) => changed(&what),
    }
}

/// The database and table oids as the transaction sees them, which the undo checks before it acts (`check_ids`). Read before the
/// COMMIT, so a drop and create under the same name right after it cannot be mistaken for this change's table. Empty when they
/// cannot be read, which only skips the later check; a write is never refused over it.
pub(in crate::db::sql) async fn ids_in(tx: &mut Tx<'_>, schema: &str, table: &str) -> PgIds {
    if !matches!(tx, Tx::Pg(_)) {
        return PgIds::default();
    }
    let sql = "SELECT (SELECT oid::text FROM pg_database WHERE datname = current_database()) AS d, to_regclass($1)::oid::text AS t";
    let rows = tx.fetch_bound(sql, &[Param::Text(D.qualified(schema, table))]).await.unwrap_or_default();
    PgIds { db: rows.first().and_then(|r| crate::db::sql::worker::text(r, "d")).unwrap_or_default(), table: rows.first().and_then(|r| crate::db::sql::worker::text(r, "t")).unwrap_or_default() }
}

/// The first of `types` the server does not know as a type name, or None. PostgreSQL before 16 raises an error for text that is
/// not a type name and later ones answer NULL, so a failed batch is taken back to a savepoint and asked one by one.
pub(in crate::db::sql) async fn unknown_type(tx: &mut Tx<'_>, types: &[String]) -> Option<String> {
    if types.len() > 1 && tx.exec_bound("SAVEPOINT quarry_types", &[]).await.is_ok() {
        let values = (1..=types.len()).map(|i| format!("(${i}::text, {i})")).collect::<Vec<_>>().join(", ");
        let params: Vec<Param> = types.iter().map(|t| Param::Text(t.clone())).collect();
        match tx.fetch_bound(&format!("SELECT s.ord::bigint AS ord, to_regtype(s.v)::text AS t FROM (VALUES {values}) AS s(v, ord)"), &params).await {
            Ok(rows) => {
                let _ = tx.exec_bound("RELEASE SAVEPOINT quarry_types", &[]).await;
                let bad = rows.iter().filter(|r| crate::db::sql::worker::text(r, "t").is_none()).filter_map(|r| crate::db::sql::structure::num(r, "ord")).min();
                return bad.and_then(|o| types.get(usize::try_from(o - 1).ok()?).cloned());
            }
            Err(_) => {
                let _ = tx.exec_bound("ROLLBACK TO SAVEPOINT quarry_types", &[]).await;
                let _ = tx.exec_bound("RELEASE SAVEPOINT quarry_types", &[]).await;
            }
        }
    }
    for ty in types {
        let said = tx.fetch_bound("SELECT to_regtype($1)::text AS t", &[Param::Text(ty.clone())]).await;
        if said.ok().and_then(|rows| rows.first().and_then(|r| crate::db::sql::worker::text(r, "t"))).is_none() {
            return Some(ty.clone());
        }
    }
    None
}

/// The message for a key the server refused for the rows already there: how many rows have no parent and a few of them, so the
/// person sees what to fix. None when there is no such row (another session fixed them since), and the server's own error is
/// shown then. The transaction is at the savepoint before the key was added, where the rows are as the server saw them.
async fn orphan_message(tx: &mut Tx<'_>, schema: &str, table: &str, fk: &ForeignKey, undo: bool) -> Option<String> {
    let rows = tx.fetch_bound(&orphans_sql(D, schema, table, fk), &[]).await.ok()?;
    let n = rows.first().and_then(|r| crate::db::sql::structure::num(r, "n")).unwrap_or(0);
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
/// Any failure leaves the transaction to roll back. Returns the ids the undo will check, read before the COMMIT.
pub(in crate::db::sql) async fn run(db: &Db, schema: &str, table: &str, steps: &[String], expect: &Expect, read_only: bool, undo: bool) -> Result<(PgIds, Option<String>), Fail> {
    // The changed table and the other tables of its keys, in one fixed order. A key takes a lock on both of its tables, and
    // the parent can be changed by another session between the plan and here, so it is locked before it is read.
    let locks: Vec<Lock> = lock_order(schema, table, expect.own_share, &expect.refs)
        .into_iter()
        .map(|(s, t, mode)| {
            let name = D.qualified(&s, &t);
            let gone = (s != schema || t != table).then(|| format!("the referenced table {name} (it is gone)"));
            Lock { name, mode, plain_only: false, gone }
        })
        .collect();
    let (now, later) = split_steps(steps);
    let now: Vec<String> = now.into_iter().cloned().collect();
    let later: Vec<String> = later.into_iter().cloned().collect();
    let names: Vec<String> = later.iter().filter_map(|s| concurrent_name(s)).map(String::from).collect();
    let ids = run_locked(db, schema, read_only, &locks, RELOAD_STRUCTURE, async |tx| {
        checked_run(tx, db, schema, table, &now, expect, undo).await?;
        // An index name belongs to the schema and not to the table, so a table, index or sequence of another table may hold it.
        // Seen here, after the transaction's own drops, it still rolls everything back instead of failing after the commit.
        for name in &names {
            let full = format!("{}.{name}", q(schema));
            let rows = tx.fetch_bound("SELECT to_regclass($1)::text AS r", &[Param::Text(full)]).await.map_err(|e| Fail::Run(se(e)))?;
            if rows.first().and_then(|r| crate::db::sql::worker::text(r, "r")).is_some() {
                return Err(Fail::Run(format!("The schema {schema} already has something called {name}: an index name belongs to the schema, not to the table. Pick another name. Nothing was changed.")));
            }
        }
        Ok(ids_in(tx, schema, table).await)
    })
    .await?;
    // The committed part stays committed when a concurrent build fails, so the failure comes back beside the ids and the
    // caller still records the undo of what ran.
    let late = if later.is_empty() { None } else { run_concurrent(db, schema, table, &later).await.err() };
    Ok((ids, late))
}

async fn checked_run(tx: &mut Tx<'_>, db: &Db, schema: &str, table: &str, steps: &[String], expect: &Expect, undo: bool) -> Result<(), Fail> {
    // Before the read and the statements alike: the plan was made from a read under this path, and the statements resolve
    // every name under it, so a type or function outside the schema is qualified in the one and found by the other.
    set_path(tx, db, schema).await.map_err(|e| Fail::Run(se(e)))?;
    let st = read_in(tx, schema, table).await.map_err(Fail::Run)?;
    let now = snapshot(&st, &expect.cols, &expect.idx, &expect.fks, &expect.checks);
    if now.len() != expect.snap.len() {
        return Err(Fail::Changed("the table".into()));
    }
    if let Some((what, _)) = expect.snap.iter().zip(&now).find(|(a, b)| a != b).map(|(a, _)| a) {
        return Err(Fail::Changed(what.clone()));
    }
    if let Some(all) = &expect.all && table_state(&st) != *all {
        return Err(Fail::Changed("an index, constraint or other object that depends on the table".into()));
    }
    // The referenced columns and unique indexes a new key leans on, read under the search_path of their own schema as the
    // plan-time read of them was.
    let mut moved = false;
    for r in expect.refs.iter().filter(|r| r.seen.is_some()) {
        let now = if r.schema == schema && r.table == table {
            ref_state(&st, &r.cols)
        } else {
            set_path(tx, db, &r.schema).await.map_err(|e| Fail::Run(se(e)))?;
            moved = true;
            ref_state(&read_in(tx, &r.schema, &r.table).await.map_err(Fail::Run)?, &r.cols)
        };
        if Some(&now) != r.seen.as_ref() {
            return Err(Fail::Changed(format!("the referenced table {}", D.qualified(&r.schema, &r.table))));
        }
    }
    if moved {
        set_path(tx, db, schema).await.map_err(|e| Fail::Run(se(e)))?;
    }
    if let Some(ty) = unknown_type(tx, &expect.types).await {
        return Err(Fail::Run(format!("{ty:?} is not a type the server knows. Write only the type, like numeric(10,2), without constraints or other column clauses. Nothing was changed.")));
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
    let added: Vec<String> = expect.checks.iter().filter(|n| steps.iter().any(|s| s.starts_with(&add_check_head(schema, table, n)))).cloned().collect();
    if !added.is_empty() && !undo {
        added_checks_immutable(tx, schema, table, &added).await?;
    }
    Ok(())
}

/// The note for a structure plan that did not run.
fn plan_failed(f: Fail, undo: bool) -> String {
    fail_text(f, |what| {
        if undo {
            format!("Undo refused: {what} changed after this change ran, so undoing it now would not put the table back.")
        } else {
            format!("Nothing ran: {what} changed since this plan was made. Reload the structure and stage the change again.")
        }
    })
}

/// The undo entry for the table, which remembers the database it ran in: a PostgreSQL connection is bound to one, and
/// the undo has to run on that one's pool.
fn action(db: &Db, conn: u64, schema: &str, table: &str, u: AlterUndo, ids: PgIds) -> UndoAction {
    let mut a = undo_action(conn, schema, table, u);
    if let UndoKind::Sql { database, ids: own, .. } = &mut a.kind {
        *database = db.pg_database();
        *own = ids;
    }
    a
}

/// Runs a recorded undo in one transaction, after the same check as an apply against how the change left the table.
pub(in crate::db::sql) async fn undo(db: &Db, a: UndoAction, u: AlterUndo, read_only: bool) -> Msg {
    let ids = match &a.kind {
        UndoKind::Sql { ids, .. } => ids.clone(),
        _ => PgIds::default(),
    };
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let expect = Expect { cols: u.cols.clone(), idx: u.idx.clone(), fks: u.fks.clone(), checks: u.checks.clone(), snap: u.expect.clone(), all: None, types: Vec::new(), refs: u.refs.clone(), orphans: u.orphans.clone(), own_share: shares_only(&u.groups.iter().flatten().cloned().collect::<Vec<_>>()) };
    let out = crate::db::sql_scope(db.conn, &schema, async { run(db, &schema, &table, &u.steps, &expect, read_only, true).await }).await;
    let unsure = matches!(out, Err(Fail::Unknown(_)));
    let res: Res<String> = out.map(|_| "Undid the structure change".to_string()).map_err(|f| plan_failed(f, true));
    // The whole undo is one transaction, so a failure ran nothing and the action stays for another try, unless the commit
    // is the one that failed, when it may have run.
    let keep = (res.is_err() && !unsure).then(|| {
        let mut k = action(db, conn, &schema, &table, u, ids);
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
            let (res, ids, late) = match out {
                Ok((ids, late)) => (Ok(()), ids, late),
                Err(f) => (Err(plan_failed(f, false)), PgIds::default(), None),
            };
            // A concurrent build that failed after the commit is an error that still carries the undo of what did run, and the
            // structure is reloaded.
            let unsure = unsure || late.is_some();
            let mut done = done;
            let undo = match (res.is_ok(), undo) {
                (true, Some(mut u)) => {
                    // The table and the tables of its keys are independent reads.
                    let want: Vec<usize> = u.refs.iter().enumerate().filter(|(_, r)| !r.cols.is_empty()).map(|(i, _)| i).collect();
                    let (st, loads) = futures_util::future::join(load(&db, &schema, &table), futures_util::future::join_all(want.iter().map(|&i| load(&db, &u.refs[i].schema, &u.refs[i].table)))).await;
                    match st {
                        Ok(st) => {
                            u.expect = snapshot(&st, &u.cols, &u.idx, &u.fks, &u.checks);
                            // The undo of a dropped key puts it back and leans on the parent as it is now.
                            let mut unread = None;
                            for (&i, loaded) in want.iter().zip(loads) {
                                match loaded {
                                    Ok(p) => u.refs[i].seen = Some(ref_state(&p, &u.refs[i].cols)),
                                    Err(err) => unread = Some(err),
                                }
                            }
                            match unread {
                                None => Some(action(&db, conn, &schema, &table, u, ids)),
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
                    }
                }
                _ => None,
            };
            Msg::SqlAltered { conn, database: db.pg_database(), schema, table, res: match late { Some(e) => Err(e), None => res.map(|_| done) }, undo, sql_mode: None, alter: true, unsure }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::structure::{ColDep, Constraint, Index, IndexCol};

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
        Column { name: name.into(), full_type: ty.into(), nullable: true, kind: Some(crate::db::sql::value::Kind::Text), ..Default::default() }
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
        let ix = IndexDef { name: "t_note".into(), kind: IndexKind::Unique, cols: vec![crate::db::sql::ddl::IdxCol { name: "note".into(), prefix: None, desc: true }], comment: "by note".into(), ..Default::default() };
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
        let spatial = IndexDef { name: "g".into(), kind: IndexKind::Fulltext, cols: vec![crate::db::sql::ddl::IdxCol { name: "note".into(), prefix: None, desc: false }], ..Default::default() };
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
        let ix = IndexDef { name: "t_note_pkey".into(), kind: IndexKind::Primary, cols: vec![crate::db::sql::ddl::IdxCol { name: "note".into(), prefix: None, desc: false }], ..Default::default() };
        let mut st = table();
        st.indexes.remove(0);
        let p = plan("t", &st, &[Change::AddIndex(ix)]).unwrap();
        assert_eq!(p.steps, ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_note_pkey\" PRIMARY KEY (\"note\")"]);
        assert_eq!(p.undo.unwrap().steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_note_pkey\"", "ALTER TABLE \"public\".\"t\" ALTER COLUMN \"note\" DROP NOT NULL"]);
    }

    fn ix_on(name: &str, cols: &[&str]) -> IndexDef {
        IndexDef { name: name.into(), cols: cols.iter().map(|c| crate::db::sql::ddl::IdxCol { name: (*c).into(), prefix: None, desc: false }).collect(), ..Default::default() }
    }

    #[test]
    fn a_create_index_carries_its_method_condition_include_and_concurrently() {
        let t = "\"public\".\"t\"";
        let base = ix_on("t_n", &["name"]);
        let add = |ix: IndexDef| steps(&[Change::AddIndex(ix)]);
        assert_eq!(add(IndexDef { method: "hash".into(), ..base.clone() }), [format!("CREATE INDEX \"t_n\" ON {t} USING hash (\"name\")")]);
        assert_eq!(add(IndexDef { kind: IndexKind::Unique, predicate: "note IS NOT NULL".into(), ..base.clone() }), [format!("CREATE UNIQUE INDEX \"t_n\" ON {t} (\"name\") WHERE note IS NOT NULL")]);
        assert_eq!(add(IndexDef { include: vec!["note".into()], ..base.clone() }), [format!("CREATE INDEX \"t_n\" ON {t} (\"name\") INCLUDE (\"note\")")]);
        let all = IndexDef { kind: IndexKind::Unique, method: "btree".into(), include: vec!["note".into()], predicate: "id > 0".into(), concurrently: true, comment: "why".into(), ..base.clone() };
        assert_eq!(add(all), [format!("CREATE UNIQUE INDEX CONCURRENTLY \"t_n\" ON {t} USING btree (\"name\") INCLUDE (\"note\") WHERE id > 0"), "COMMENT ON INDEX \"public\".\"t_n\" IS 'why'".to_string()]);
        // a unique constraint can carry INCLUDE columns, and nothing else of these
        let uq = IndexDef { kind: IndexKind::Unique, constraint: true, include: vec!["note".into()], ..base.clone() };
        assert_eq!(add(uq.clone()), [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_n\" UNIQUE (\"name\") INCLUDE (\"note\")")]);
        let st = table();
        let refused = |ix: IndexDef| plan("t", &st, &[Change::AddIndex(ix)]).err().unwrap_or_default();
        assert!(refused(IndexDef { method: "hash".into(), include: vec!["note".into()], ..base.clone() }).contains("INCLUDE"));
        assert!(refused(IndexDef { method: "gin".into(), kind: IndexKind::Unique, ..base.clone() }).contains("UNIQUE"));
        assert!(refused(IndexDef { method: "bogus".into(), ..base.clone() }).contains("not an index method"));
        assert!(refused(IndexDef { concurrently: true, ..uq.clone() }).contains("CONCURRENTLY"));
        assert!(refused(IndexDef { predicate: "id > 0".into(), ..uq }).contains("partial"));
        let mut desc = ix_on("t_d", &["name"]);
        desc.cols[0].desc = true;
        assert!(refused(IndexDef { method: "gist".into(), ..desc }).contains("sort order"));
    }

    #[test]
    fn an_index_condition_stays_one_expression() {
        for ok in ["qty > 1", "(note IS NOT NULL)", "status = 'open;closed'", "lower(name) <> 'x'"] {
            assert!(check_predicate_pg(ok).is_ok(), "{ok}");
        }
        for bad in ["", "qty > 1; DROP TABLE t", "qty > 1 -- c", "qty > 1 /* c */", "(qty > 1", "qty > 1)", "name = 'open", "qty > 1) WITH (fillfactor = 10"] {
            assert!(check_predicate_pg(bad).is_err(), "{bad}");
        }
        // the plan refuses the same text before any statement exists
        let ix = IndexDef { predicate: "qty > 1; DROP TABLE t".into(), ..ix_on("t_n", &["name"]) };
        assert!(plan("t", &table(), &[Change::AddIndex(ix)]).err().unwrap_or_default().contains("index condition"));
    }

    #[test]
    fn a_concurrent_index_runs_after_the_transaction_with_its_comment_and_is_undone_by_a_drop() {
        let ix = IndexDef { concurrently: true, comment: "why".into(), ..ix_on("t_n", &["name"]) };
        let p = plan("t", &table(), &[Change::DropIndex("t_name".into()), Change::AddIndex(ix)]).unwrap();
        let (now, later) = split_steps(&p.steps);
        assert_eq!(now.len(), 1);
        assert!(now[0].starts_with("DROP INDEX"));
        assert_eq!(later.len(), 2);
        assert!(later[0].starts_with("CREATE INDEX CONCURRENTLY \"t_n\"") && later[1].starts_with("COMMENT ON INDEX"));
        let sql = p.sql();
        let (drop, note, create) = (sql.find("DROP INDEX").unwrap(), sql.find("cannot run inside a transaction").unwrap(), sql.find("CONCURRENTLY \"t_n\"").unwrap());
        assert!(drop < note && note < create, "{sql}");
        // the undo is a plain DROP, which runs in the transaction
        let undo = p.undo.unwrap();
        assert_eq!(undo.steps[0], "DROP INDEX \"public\".\"t_n\"");
        assert!(undo.steps.iter().all(|s| split_steps(std::slice::from_ref(s)).1.is_empty()), "{:?}", undo.steps);
        // without a concurrent index the preview is the plain script
        assert!(!plan("t", &table(), &[Change::DropIndex("t_name".into())]).unwrap().sql().contains("CONCURRENTLY"));
        // a name with a quote in it is read to its end
        assert_eq!(concurrent_name("CREATE INDEX CONCURRENTLY \"a\"\" ON \" ON t (x)"), Some("\"a\"\" ON \""));
    }

    #[test]
    fn any_index_can_be_dropped_and_undone_from_the_servers_own_text() {
        let raw = "CREATE INDEX t_part ON public.t USING btree (name text_pattern_ops) WHERE (note IS NOT NULL)";
        let mut st = table();
        st.indexes.push(Index { complex: true, ..index("t_part", raw, &["name"]) });
        let p = plan("t", &st, &[Change::DropIndex("t_part".into())]).unwrap();
        assert_eq!(p.steps, ["DROP INDEX \"public\".\"t_part\"".to_string()]);
        assert_eq!(p.undo.unwrap().steps, [raw.to_string()]);
        // a constraint's index is not made by CREATE INDEX, so its drop is still not undoable
        let mut st = table();
        st.indexes.push(Index { complex: true, constraint: true, unique: true, ..index("t_ex", "CREATE UNIQUE INDEX t_ex ON public.t USING btree (name) INCLUDE (note) WITH (fillfactor=70)", &["name"]) });
        assert!(plan("t", &st, &[Change::DropIndex("t_ex".into())]).unwrap().undo.is_err());
    }

    #[test]
    fn a_partial_or_covering_index_is_read_into_the_form_and_written_back_the_same() {
        let ix = Index { predicate: "(note IS NOT NULL)".into(), include: vec!["note".into()], ..index("t_p", "CREATE INDEX t_p ON public.t USING btree (name) INCLUDE (note) WHERE (note IS NOT NULL)", &["name"]) };
        let d = IndexDef::from_index(&ix);
        assert!(!d.functional);
        assert_eq!((d.predicate.as_str(), d.include.as_slice()), ("(note IS NOT NULL)", &["note".to_string()][..]));
        assert_eq!(steps(&[Change::AddIndex(d)]), ["CREATE INDEX \"t_p\" ON \"public\".\"t\" (\"name\") INCLUDE (\"note\") WHERE (note IS NOT NULL)".to_string()]);
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
        let ix = IndexDef { name: "t_u".into(), kind: IndexKind::Unique, constraint: true, deferrable: true, deferred: true, con_comment: "one per name".into(), cols: vec![crate::db::sql::ddl::IdxCol { name: "name".into(), prefix: None, desc: false }], ..Default::default() };
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
        assert!(crate::db::sql::ddl::warnings(&st, &with(Dv::Expr("gen_random_uuid()".into()))).is_empty(), "no rows known, nothing to rewrite");
        st.status.rows = Some(3);
        let w = crate::db::sql::ddl::warnings(&st, &with(Dv::Expr("gen_random_uuid()".into())));
        assert!(w.iter().any(|x| x.contains("once for each of the table's rows") && x.contains("exclusive lock")), "{w:?}");
        assert!(crate::db::sql::ddl::warnings(&st, &with(Dv::Expr("'x'::text".into()))).is_empty());
        assert!(crate::db::sql::ddl::warnings(&st, &with(Dv::Value("x".into()))).is_empty());
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

    fn chk(name: &str, expr: &str) -> CheckConstraint {
        CheckConstraint { name: name.into(), expr: expr.into(), ..Default::default() }
    }

    #[test]
    fn a_check_is_added_and_dropped_as_a_constraint() {
        let t = "\"public\".\"t\"";
        assert_eq!(steps(&[Change::AddCheck(chk("t_name_check", "length(name) > 0"))]), [format!("ALTER TABLE {t} ADD CONSTRAINT \"t_name_check\" CHECK (length(name) > 0)")]);
        let both = CheckConstraint { not_valid: true, no_inherit: true, ..chk("Odd \"name\"", " price > 0 ") };
        assert_eq!(steps(&[Change::AddCheck(both)]), [format!("ALTER TABLE {t} ADD CONSTRAINT \"Odd \"\"name\"\"\" CHECK (price > 0) NO INHERIT NOT VALID")]);
        assert!(steps(&[Change::AddCheck(CheckConstraint { not_enforced: true, not_valid: true, ..chk("c", "a > 0") })])[0].ends_with("CHECK (a > 0) NOT ENFORCED"));
        // a condition that is not one expression, one that acts outside the row, an empty one and a long name are refused
        for bad in [chk("c", "a > 0); DROP TABLE t; --"), chk("c", "a > 0 -- x"), chk("c", "pg_advisory_lock(1) IS NOT NULL"), chk("c", ""), chk(&"n".repeat(64), "a > 0"), chk("", "a > 0")] {
            assert!(plan("t", &table(), &[Change::AddCheck(bad.clone())]).is_err(), "{bad:?}");
        }
        let mut st = table();
        st.checks = vec![chk("T_chk", "a > 0")];
        assert_eq!(plan("t", &st, &[Change::DropCheck("T_chk".into())]).unwrap().steps, [format!("ALTER TABLE {t} DROP CONSTRAINT \"T_chk\"")]);
        // PostgreSQL tells two names apart that differ only in case
        assert!(plan("t", &st, &[Change::DropCheck("t_chk".into())]).is_err());
        assert!(plan("t", &st, &[Change::AddCheck(chk("t_chk", "a > 1"))]).is_ok());
        assert!(plan("t", &st, &[Change::AddCheck(chk("T_chk", "a > 1"))]).unwrap().undo.is_err());
    }

    #[test]
    fn the_undo_of_a_check_change_is_the_check_as_it_was() {
        let mut st = table();
        let old = CheckConstraint { not_valid: true, ..chk("t_chk", "a > 0") };
        st.checks = vec![old.clone()];
        let undo = plan("t", &st, &[Change::DropCheck("t_chk".into())]).unwrap().undo.unwrap();
        assert_eq!(undo.groups, [vec![Change::AddCheck(old.clone())]]);
        assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_chk\" CHECK (a > 0) NOT VALID"]);
        let undo = plan("t", &table(), &[Change::AddCheck(old.clone())]).unwrap().undo.unwrap();
        assert_eq!(undo.steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_chk\""]);
        // an edit is a drop and an add of the same name: undone by dropping the new one and adding the old one back
        let new = chk("t_chk", "a > 5");
        let p = plan("t", &st, &[Change::DropCheck("t_chk".into()), Change::AddCheck(new)]).unwrap();
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.undo.unwrap().steps, ["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_chk\"", "ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"t_chk\" CHECK (a > 0) NOT VALID"]);
        // the plan checks the constraint again under the lock, and adding one takes the exclusive lock
        let e = plan("t", &table(), &[Change::AddCheck(chk("c", "a > 0"))]).unwrap().expect.unwrap();
        assert_eq!(e.checks, ["c"]);
        assert!(!e.own_share);
    }

    #[test]
    fn a_check_is_read_from_the_catalog_text() {
        let con = |def: &str| Constraint { name: "c".into(), kind: 'c', def: def.into() };
        let k = CheckConstraint::from_pg(&con("CHECK ((total >= (0)::numeric))")).unwrap();
        assert_eq!((k.expr.as_str(), k.not_valid, k.no_inherit, k.not_enforced), ("(total >= (0)::numeric)", false, false, false));
        let k = CheckConstraint::from_pg(&con("CHECK (status = ANY (ARRAY['a'::text, 'b)'::text])) NO INHERIT NOT VALID")).unwrap();
        assert_eq!((k.expr.as_str(), k.not_valid, k.no_inherit), ("status = ANY (ARRAY['a'::text, 'b)'::text])", true, true));
        // a quote that is escaped in an E string does not end the string
        let k = CheckConstraint::from_pg(&con("CHECK (note <> E'it\\'s)')")).unwrap();
        assert_eq!(k.expr, "note <> E'it\\'s)'");
        assert!(CheckConstraint::from_pg(&con("CHECK ((a > 0)) NOT ENFORCED")).unwrap().not_enforced);
        assert!(CheckConstraint::from_pg(&Constraint { kind: 'u', ..con("UNIQUE (a)") }).is_none());
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
        let snap = snapshot(&st, &[], &[], &["t_fk".to_string(), "T_fk".to_string()], &[]);
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
