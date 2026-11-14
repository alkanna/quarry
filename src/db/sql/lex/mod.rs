//! A small SQL tokenizer (MySQL, MariaDB and PostgreSQL), the statement splitter and the read-only guard. Shared by
//! the worker, the highlighter and autocomplete, so all of them agree on where strings and comments end.

use super::Dialect;
use std::ops::Range;

mod pg;
pub(super) use pg::dollar_quote;
pub(crate) use pg::{refused_call, refused_calls};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Space,
    Comment,
    /// `/*!`, `/*!80000` or MariaDB's `/*M!`: the server runs what follows, so it is code.
    ExecOpen,
    /// The `*/` that ends an executable comment.
    ExecClose,
    Word,
    /// A backtick identifier, quotes included.
    Quoted,
    /// A '...' or "..." string, quotes included.
    Str,
    Punct,
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// How the server may read the text. sql_mode and the server version change where strings and
/// comments end, and the guard cannot know them, so it checks every combination.
#[derive(Clone, Copy)]
struct Opts {
    /// A backslash escapes the next char inside quotes (off under NO_BACKSLASH_ESCAPES).
    backslash: bool,
    /// Double quotes hold identifiers with no backslash escape (ANSI_QUOTES).
    ansi: bool,
    /// Executable comments are skipped like plain ones, as when their version is above the server's
    /// (and MySQL treats `/*M!` as a plain comment).
    skip_exec: bool,
    /// With PostgreSQL text, `backslash` means a plain '...' honours backslashes (standard_conforming_strings off).
    dialect: Dialect,
}

const DEFAULT: Opts = Opts { backslash: true, ansi: false, skip_exec: false, dialect: Dialect::MySql };
const DEFAULT_PG: Opts = Opts { backslash: false, ansi: false, skip_exec: false, dialect: Dialect::Postgres };

fn default_for(d: Dialect) -> Opts {
    if d == Dialect::Postgres { DEFAULT_PG } else { DEFAULT }
}

fn opts(d: Dialect, backslash: bool, ansi: bool) -> Opts {
    Opts { backslash, ansi, skip_exec: false, dialect: d }
}

/// Index just past the closing quote of the quoted text that opens at `i`. Unterminated text runs
/// to the end.
fn quote_end(b: &[u8], i: usize, esc: bool) -> usize {
    let (n, c) = (b.len(), b[i]);
    let mut i = i + 1;
    while i < n {
        if b[i] == b'\\' && esc {
            i += 2;
        } else if b[i] == c {
            if b.get(i + 1) == Some(&c) {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    n
}

/// Whether the PostgreSQL read-only check refuses `name` as a called function, so autocomplete never offers it.
pub fn pg_refuses_function(name: &str) -> bool {
    pg::refused_function(&name.to_ascii_lowercase())
}

/// Splits text into tokens that cover it completely. Delimiters are ASCII, so every range sits on a
/// char boundary. Unterminated strings and comments run to the end of the text.
pub fn tokens(text: &str, d: Dialect) -> Vec<(Tok, Range<usize>)> {
    lex(text, default_for(d))
}

fn lex(text: &str, o: Opts) -> Vec<(Tok, Range<usize>)> {
    if o.dialect == Dialect::Postgres {
        return pg::lex(text, o.backslash);
    }
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut exec = false;
    let mut i = 0;
    while i < n {
        let s = i;
        let c = b[i];
        let next = b.get(i + 1).copied();
        let tok = if c.is_ascii_whitespace() {
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            Tok::Space
        } else if c == b'\'' || c == b'"' || c == b'`' {
            // Backticks have no backslash escape, quotes do unless the mode says otherwise.
            let ident = c == b'`' || (c == b'"' && o.ansi);
            i = quote_end(b, i, o.backslash && !ident);
            if ident { Tok::Quoted } else { Tok::Str }
        } else if c == b'#' || (c == b'-' && next == Some(b'-') && b.get(i + 2).is_none_or(|x| x.is_ascii_whitespace() || x.is_ascii_control())) {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            Tok::Comment
        } else if c == b'/' && next == Some(b'*') {
            let mut j = i + 2;
            if b.get(j) == Some(&b'M') && b.get(j + 1) == Some(&b'!') {
                j += 1;
            }
            if b.get(j) == Some(&b'!') && !o.skip_exec {
                j += 1;
                while j < n && b[j].is_ascii_digit() {
                    j += 1;
                }
                exec = true;
                i = j;
                Tok::ExecOpen
            } else {
                i = text[i + 2..].find("*/").map_or(n, |p| i + 2 + p + 2);
                Tok::Comment
            }
        } else if exec && c == b'*' && next == Some(b'/') {
            exec = false;
            i += 2;
            Tok::ExecClose
        } else if c == b'@' {
            // A user variable is @ glued to a name or a quoted name. A lone @ is punctuation, so a
            // comment between the two cannot pass as a variable.
            let mut j = i;
            while j < n && j < i + 2 && b[j] == b'@' {
                j += 1;
            }
            if j < n && is_word(b[j]) {
                while j < n && is_word(b[j]) {
                    j += 1;
                }
                i = j;
                Tok::Word
            } else if j < n && (b[j] == b'`' || b[j] == b'\'' || b[j] == b'"') {
                i = quote_end(b, j, o.backslash && b[j] != b'`' && !(b[j] == b'"' && o.ansi));
                Tok::Word
            } else {
                i += 1;
                Tok::Punct
            }
        } else if is_word(c) {
            while i < n && is_word(b[i]) {
                i += 1;
            }
            Tok::Word
        } else {
            i += 1;
            Tok::Punct
        };
        out.push((tok, s..i));
    }
    out
}

/// Each statement as (range without the `;`, end including the `;`). Empty ones are dropped.
fn split(text: &str, d: Dialect) -> Vec<(Range<usize>, usize)> {
    split_with(text, default_for(d))
}

fn split_with(text: &str, o: Opts) -> Vec<(Range<usize>, usize)> {
    let mut out = Vec::new();
    let (mut start, mut last, mut code) = (None, 0, false);
    for (t, r) in lex(text, o) {
        match t {
            Tok::Space => {}
            Tok::Punct if &text[r.clone()] == ";" => {
                if let (Some(s), true) = (start, code) {
                    out.push((s..last, r.end));
                }
                (start, code) = (None, false);
            }
            // A trailing comment belongs to no statement, so it never moves the end.
            Tok::Comment => {
                start.get_or_insert(r.start);
            }
            _ => {
                start.get_or_insert(r.start);
                last = r.end;
                code |= matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct);
            }
        }
    }
    if let (Some(s), true) = (start, code) {
        out.push((s..last, text.len()));
    }
    out
}

/// Splits on `;` outside strings and comments. Ranges are byte offsets into `text`, trimmed of
/// surrounding whitespace, so the editor can map a caret to a statement.
pub fn split_statements(text: &str, d: Dialect) -> Vec<(Range<usize>, &str)> {
    split(text, d).into_iter().map(|(r, _)| (r.clone(), &text[r])).collect()
}

/// The `?` placeholders (`$1`, `$2` on PostgreSQL) and the statement count of `text` as a server with these quote
/// rules reads it. The worker compares two readings, because a text that reads differently is not safe to fill.
pub fn placeholders(text: &str, d: Dialect, backslash: bool, ansi: bool) -> (Vec<usize>, usize) {
    let o = opts(d, backslash, ansi);
    let at = lex(text, o).into_iter().filter(|(t, r)| if o.dialect == Dialect::Postgres { *t == Tok::Word && pg::param_number(&text[r.clone()]).is_some() } else { *t == Tok::Punct && &text[r.clone()] == "?" }).map(|(_, r)| r.start).collect();
    (at, split_with(text, o).len())
}

/// Like `tokens`, but read with the server's own quote rules.
pub fn tokens_with(text: &str, d: Dialect, backslash: bool, ansi: bool) -> Vec<(Tok, Range<usize>)> {
    lex(text, opts(d, backslash, ansi))
}

/// The number of a `$n` parameter token, for PostgreSQL text.
pub fn param_number(token: &str) -> Option<usize> {
    pg::param_number(token)
}

/// Every way a server may read `text`: backslash escapes on and off, ANSI_QUOTES on and off, and executable
/// comments as code and as comments. A text that lexes the same in all of them means the same to any server.
pub fn every_reading(text: &str, d: Dialect) -> Vec<Vec<(Tok, Range<usize>)>> {
    (0..readings(d)).map(|n| nth_reading(text, d, n)).collect()
}

/// How many readings MySQL text has.
pub const READINGS: usize = 8;

/// How many readings `every_reading` holds. PostgreSQL has two: standard_conforming_strings on and off.
pub fn readings(d: Dialect) -> usize {
    if d == Dialect::Postgres { 2 } else { READINGS }
}

/// One of the `readings` of `text`, so a caller that only needs one at a time never holds all of them.
pub fn nth_reading(text: &str, d: Dialect, n: usize) -> Vec<(Tok, Range<usize>)> {
    if d == Dialect::Postgres {
        return lex(text, Opts { backslash: n & 1 != 0, ..DEFAULT_PG });
    }
    lex(text, Opts { backslash: n & 4 == 0, ansi: n & 2 != 0, skip_exec: n & 1 != 0, dialect: Dialect::MySql })
}

/// The database a `USE name` switches to, with the name unquoted.
pub fn use_target(stmt: &str, d: Dialect) -> Option<String> {
    // PostgreSQL has no USE: a connection is bound to one database.
    if d == Dialect::Postgres {
        return None;
    }
    // Most statements are not USE, and some are megabytes long: look at their first characters before lexing.
    let head = stmt.trim_start();
    if !matches!(head.as_bytes().first(), Some(b'/' | b'-' | b'#')) && !head.get(..3).is_some_and(|h| h.eq_ignore_ascii_case("use")) {
        return None;
    }
    let mut it = tokens(stmt, d).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment));
    let (t, r) = it.next()?;
    if t != Tok::Word || !stmt[r].eq_ignore_ascii_case("use") {
        return None;
    }
    let (t, r) = it.next()?;
    let name = &stmt[r];
    match t {
        Tok::Word => Some(name.to_string()),
        Tok::Quoted => Some(name.trim_matches('`').replace("``", "`")),
        _ => None,
    }
}

/// Statements counted under the server's own quote rules.
pub fn count_with(text: &str, d: Dialect, backslash: bool, ansi: bool) -> usize {
    split_with(text, opts(d, backslash, ansi)).len()
}

/// The statement the caret belongs to. A caret right after a `;` stays with that statement, and one
/// past the last statement picks the last one.
pub fn statement_at(text: &str, caret: usize, d: Dialect) -> Option<Range<usize>> {
    let all = split(text, d);
    all.iter().find(|(_, semi)| *semi >= caret).or(all.last()).map(|(r, _)| r.clone())
}

/// Words of a statement outside parentheses, in capitals. None when the text holds a comment the server may run.
fn top_words(stmt: &str, d: Dialect) -> Option<Vec<String>> {
    let (mut depth, mut out) = (0i32, Vec::new());
    for (t, r) in tokens(stmt, d) {
        match t {
            Tok::ExecOpen | Tok::ExecClose => return None,
            Tok::Punct if &stmt[r.clone()] == "(" => depth += 1,
            Tok::Punct if &stmt[r.clone()] == ")" => depth -= 1,
            Tok::Word if depth == 0 => out.push(stmt[r].to_ascii_uppercase()),
            _ => {}
        }
    }
    Some(out)
}

/// Whether a statement can sit inside `SELECT * FROM (...) AS q LIMIT ...` to be read a page at a time:
/// one SELECT, WITH, TABLE or VALUES that writes nothing and has no INTO or locking clause.
pub fn pageable(stmt: &str, d: Dialect) -> bool {
    // A data-modifying CTE sits inside parentheses, where top_words does not look, and the server refuses one that is
    // not at the top level of the statement.
    if d == Dialect::Postgres && tokens(stmt, d).iter().any(|(t, r)| *t == Tok::Word && ["INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|w| stmt[r.clone()].eq_ignore_ascii_case(w))) {
        return false;
    }
    let Some(w) = top_words(stmt, d) else { return false };
    w.first().is_some_and(|f| ["SELECT", "WITH", "TABLE", "VALUES"].contains(&f.as_str())) && !w.iter().any(|x| ["INTO", "UPDATE", "DELETE", "INSERT", "REPLACE", "LOCK", "SHARE"].contains(&x.as_str()))
}

/// Whether a PostgreSQL statement controls the transaction around it: BEGIN, START, COMMIT, END, ROLLBACK, ABORT,
/// SAVEPOINT, RELEASE or PREPARE TRANSACTION. Behind a pooler each statement runs in a transaction of Quarry's own.
pub fn transaction_control(stmt: &str, d: Dialect) -> bool {
    if d != Dialect::Postgres {
        return false;
    }
    let w = head_words(stmt, d, 2);
    match w.first().map(String::as_str) {
        Some("BEGIN" | "START" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "SAVEPOINT" | "RELEASE") => true,
        Some("PREPARE") => w.get(1).map(String::as_str) == Some("TRANSACTION"),
        _ => false,
    }
}

/// The first `n` words of a statement in upper case, for a check that only needs how it starts. A script statement can be a
/// multi-megabyte INSERT, so only the start is lexed when it holds enough words; the last two tokens of that start are left out
/// because the cut may have split them. Anything short of `n` words in the start reads the whole statement, so the answer is
/// the one a full read gives.
pub(super) fn head_words(stmt: &str, d: Dialect, n: usize) -> Vec<String> {
    const HEAD: usize = 2048;
    let words = |text: &str, skip_tail: usize| -> Vec<String> {
        let toks = tokens(text, d);
        let keep = toks.len().saturating_sub(skip_tail);
        toks[..keep].iter().filter(|(t, _)| *t == Tok::Word).map(|(_, r)| text[r.clone()].to_ascii_uppercase()).take(n).collect()
    };
    if stmt.len() > HEAD {
        let w = words(&stmt[..stmt.floor_char_boundary(HEAD)], 2);
        if w.len() >= n {
            return w;
        }
    }
    words(stmt, 0)
}

/// Whether a PostgreSQL statement leaves state on the session after its transaction commits. Behind a transaction
/// pooler the backend goes to other clients next, so a session SET, a temp table, a named prepared statement, a held
/// cursor or a LISTEN would reach them. set_config is refused whatever its third argument, which the lexer cannot read
/// reliably.
pub fn session_state(stmt: &str, d: Dialect) -> bool {
    if d != Dialect::Postgres {
        return false;
    }
    let words: Vec<String> = tokens(stmt, d).into_iter().filter(|(t, _)| *t == Tok::Word || *t == Tok::Quoted).map(|(_, r)| stmt[r].trim_matches('"').to_ascii_uppercase()).collect();
    let at = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let first = match at(0) {
        "SET" => !matches!(at(1), "LOCAL" | "TRANSACTION" | "CONSTRAINTS" | "CONSTRAINT"),
        "RESET" | "DISCARD" | "LISTEN" | "UNLISTEN" | "PREPARE" | "DEALLOCATE" | "LOAD" => true,
        "DECLARE" => words.windows(2).any(|w| w[0] == "WITH" && w[1] == "HOLD"),
        // A DO block is code the lexer cannot read: it can take an advisory lock or make a temporary table.
        // The dump's own blocks: types and domains, and schemas made only when missing.
        "DO" => !quarry_guard_block(stmt) && !crate::db::sql::pg::dump::is_schema_block(stmt.trim()),
        "CREATE" => matches!(at(1), "TEMP" | "TEMPORARY") || (matches!(at(1), "LOCAL" | "GLOBAL") && matches!(at(2), "TEMP" | "TEMPORARY")),
        _ => false,
    };
    // Anything made in pg_temp (CREATE, SELECT INTO, CREATE TABLE AS, a function) stays on the backend, so the name is
    // refused wherever it appears in the statement, not only after a leading CREATE.
    let temp_schema = words.iter().any(|w| w == "PG_TEMP" || w.starts_with("PG_TEMP_"));
    first || temp_schema || words.iter().any(|w| w == "SET_CONFIG") || words.windows(2).any(|w| w[0] == "INTO" && matches!(w[1].as_str(), "TEMP" | "TEMPORARY")) || pg::session_function(stmt)
}

/// The one DO block Quarry writes itself in a dump of some tables: a single CREATE TYPE, CREATE DOMAIN or ALTER DOMAIN
/// that ignores "already exists". It is let through behind a pooler, and only when what is inside passes every check
/// again, because any other DO block can do what the checks refuse.
fn quarry_guard_block(stmt: &str) -> bool {
    const HEAD: &str = "BEGIN ";
    const TAIL: &str = "; EXCEPTION WHEN duplicate_object THEN NULL; END";
    let toks: Vec<(Tok, &str)> = tokens(stmt, Dialect::Postgres).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).map(|(t, r)| (t, &stmt[r])).collect();
    let [(Tok::Word, _), (Tok::Str, body)] = toks.as_slice() else { return false };
    let Some(tag_end) = body.strip_prefix('$').and_then(|r| r.find('$')).map(|i| i + 2) else { return false };
    let tag = &body[..tag_end];
    let Some(inner) = body[tag_end..].strip_suffix(tag) else { return false };
    let Some(mid) = inner.strip_prefix(HEAD).and_then(|m| m.strip_suffix(TAIL)) else { return false };
    let w: Vec<String> = tokens(mid, Dialect::Postgres).into_iter().filter(|(t, r)| *t == Tok::Word || (*t == Tok::Punct && &mid[r.clone()] == ";")).map(|(_, r)| mid[r].to_ascii_uppercase()).collect();
    let shape = matches!((w.first().map(String::as_str), w.get(1).map(String::as_str)), (Some("CREATE"), Some("TYPE" | "DOMAIN")) | (Some("ALTER"), Some("DOMAIN")));
    shape && !w.iter().any(|x| x == ";") && !session_state(mid, Dialect::Postgres)
}

/// Whether a PostgreSQL statement is one the server refuses inside a transaction block. Behind a pooler every statement
/// is wrapped in a transaction of Quarry's own, so these can never run there. ALTER TYPE ... ADD VALUE and REFRESH
/// MATERIALIZED VIEW CONCURRENTLY are not listed because supported servers run them in a block. Only the first statement
/// of the text is read, like `transaction_control`.
pub fn needs_no_transaction(stmt: &str, d: Dialect) -> bool {
    if d != Dialect::Postgres {
        return false;
    }
    // Only these statements can need it, so any other, a big INSERT above all, is not read past its first word.
    if !matches!(head_words(stmt, d, 1).first().map(String::as_str), Some("VACUUM" | "CREATE" | "DROP" | "REINDEX" | "ALTER" | "CLUSTER")) {
        return false;
    }
    let w: Vec<String> = tokens(stmt, d).into_iter().take_while(|(t, r)| !(*t == Tok::Punct && &stmt[r.clone()] == ";")).filter(|(t, _)| *t == Tok::Word).map(|(_, r)| stmt[r].to_ascii_uppercase()).collect();
    let at = |i: usize| w.get(i).map(String::as_str).unwrap_or("");
    let has = |word: &str| w.iter().any(|x| x == word);
    match at(0) {
        "VACUUM" => true,
        "CREATE" => matches!(at(1), "DATABASE" | "TABLESPACE" | "SUBSCRIPTION") || (at(1) == "INDEX" && at(2) == "CONCURRENTLY") || (at(1) == "UNIQUE" && at(2) == "INDEX" && at(3) == "CONCURRENTLY"),
        "DROP" => matches!(at(1), "DATABASE" | "TABLESPACE" | "SUBSCRIPTION") || (at(1) == "INDEX" && at(2) == "CONCURRENTLY"),
        // The option list of REINDEX (CONCURRENTLY) comes before the object kind. SCHEMA, DATABASE and SYSTEM refuse a block with or without it.
        "REINDEX" => has("CONCURRENTLY") || matches!(at(1), "SCHEMA" | "DATABASE" | "SYSTEM"),
        "ALTER" => at(1) == "SYSTEM" || (at(1) == "DATABASE" && has("TABLESPACE")) || (at(1) == "TABLE" && has("DETACH") && has("PARTITION") && has("CONCURRENTLY")),
        // CLUSTER of one table runs in a block; CLUSTER alone, or with only VERBOSE, does not.
        "CLUSTER" => w.len() == 1 || (w.len() == 2 && at(1) == "VERBOSE"),
        _ => false,
    }
}

/// Whether the statement orders its rows, which keeps rows from moving between pages.
pub fn has_order_by(stmt: &str, d: Dialect) -> bool {
    top_words(stmt, d).is_some_and(|w| w.windows(2).any(|p| p[0] == "ORDER" && p[1] == "BY"))
}

/// The statement without its end: spaces and `;`.
fn bare(stmt: &str) -> &str {
    stmt.trim_end_matches(|c: char| c.is_whitespace() || c == ';')
}

/// One page of `stmt`, a row more than `size` so the caller sees whether another page follows. The
/// newlines keep a trailing `--` comment of the statement from swallowing the closing parenthesis.
pub fn page_sql(stmt: &str, page: usize, size: usize) -> String {
    format!("SELECT * FROM (\n{}\n) AS q LIMIT {} OFFSET {}", bare(stmt), size + 1, page * size)
}

/// The row count of `stmt`.
pub fn count_sql(stmt: &str) -> String {
    format!("SELECT COUNT(*) FROM (\n{}\n) AS q", bare(stmt))
}

type Code<'a> = (Tok, &'a str);

fn is(t: &Code, kw: &str) -> bool {
    t.0 == Tok::Word && t.1.eq_ignore_ascii_case(kw)
}

fn punct(t: &Code, p: &str) -> bool {
    t.0 == Tok::Punct && t.1 == p
}

const REFUSED: &str = "Unlock writes first.";

/// Allow-list on the first keyword. The session's READ ONLY transaction is the server-side guard; this
/// check refuses what that guard does not cover, for example SET SESSION or INTO OUTFILE, so neither
/// is the only boundary. Several statements are all checked.
/// The server may or may not run `/*! */` text and may read quotes differently, so the text must pass
/// under every combination of those readings. The default reading goes first for its messages.
pub fn read_only_check(stmt: &str, d: Dialect) -> Result<(), String> {
    if d == Dialect::Postgres {
        return pg::read_only_check(stmt);
    }
    for skip_exec in [false, true] {
        for ansi in [false, true] {
            for backslash in [true, false] {
                let code: Vec<Code> = lex(stmt, Opts { backslash, ansi, skip_exec, dialect: Dialect::MySql }).into_iter().filter(|(t, _)| matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct)).map(|(t, r)| (t, &stmt[r])).collect();
                code.split(|t| punct(t, ";")).try_for_each(check_one)?;
            }
        }
    }
    Ok(())
}

/// True when any statement starts with PREPARE, EXECUTE or DEALLOCATE. A prepared statement is built
/// from a string at run time, so it could hold a SET that `changes_escaping` cannot see. Checked under
/// every reading of the text, like the read-only guard.
pub fn uses_prepared(stmt: &str, d: Dialect) -> bool {
    if d == Dialect::Postgres {
        return pg::uses_prepared(stmt);
    }
    for skip_exec in [false, true] {
        for ansi in [false, true] {
            for backslash in [true, false] {
                let code: Vec<Code> = lex(stmt, Opts { backslash, ansi, skip_exec, dialect: Dialect::MySql }).into_iter().filter(|(t, _)| matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct)).map(|(t, r)| (t, &stmt[r])).collect();
                if code.split(|t| punct(t, ";")).any(|part| part.first().is_some_and(|t| is(t, "PREPARE") || is(t, "EXECUTE") || is(t, "DEALLOCATE"))) {
                    return true;
                }
            }
        }
    }
    false
}

/// True for a SET that changes how the server reads literals (character set, SQL mode). Quarry
/// escapes values for the mode it saw at connect, so a script must not move it. Checked under every
/// reading of the text, like the read-only guard.
pub fn changes_escaping(stmt: &str, d: Dialect) -> bool {
    if d == Dialect::Postgres {
        return pg::changes_escaping(stmt);
    }
    const VARS: [&str; 3] = ["character_set_client", "character_set_connection", "sql_mode"];
    const HEADS: [&str; 3] = ["names", "charset", "character"];
    for skip_exec in [false, true] {
        for ansi in [false, true] {
            for backslash in [true, false] {
                let code: Vec<Code> = lex(stmt, Opts { backslash, ansi, skip_exec, dialect: Dialect::MySql }).into_iter().filter(|(t, _)| matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct)).map(|(t, r)| (t, &stmt[r])).collect();
                for part in code.split(|t| punct(t, ";")) {
                    if !part.first().is_some_and(|t| is(t, "SET")) {
                        continue;
                    }
                    let mut depth = 0i32;
                    let mut target = true;
                    for t in &part[1..] {
                        if punct(t, "(") {
                            depth += 1;
                        } else if punct(t, ")") {
                            depth -= 1;
                        } else if depth == 0 && punct(t, ",") {
                            target = true;
                        } else if depth == 0 && (punct(t, "=") || punct(t, ":")) {
                            target = false;
                        } else if target && matches!(t.0, Tok::Word | Tok::Quoted) {
                            let w = t.1.trim_start_matches("@@").trim_matches('`').to_ascii_lowercase();
                            if VARS.contains(&w.as_str()) || HEADS.contains(&w.as_str()) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

fn check_one(part: &[Code]) -> Result<(), String> {
    if part.windows(2).any(|w| is(&w[0], "INTO") && (is(&w[1], "OUTFILE") || is(&w[1], "DUMPFILE"))) {
        return Err("INTO OUTFILE writes a file on the server and is not allowed.".into());
    }
    // A READ ONLY transaction may not stop MariaDB sequences from advancing.
    let seq = part.iter().enumerate().any(|(i, t)| {
        let call = part.get(i + 1).is_some_and(|n| punct(n, "("));
        let dotted = i > 0 && punct(&part[i - 1], ".");
        ((is(t, "SETVAL") || is(t, "NEXTVAL")) && (call || dotted)) || (is(t, "NEXT") && part.get(i + 1).is_some_and(|n| is(n, "VALUE")) && part.get(i + 2).is_some_and(|n| is(n, "FOR")))
    });
    if seq {
        return Err("Advancing a sequence changes data and is not allowed on a read-only connection.".into());
    }
    // A statement may open with parentheses, as in (SELECT 1) UNION (SELECT 2).
    let at = part.iter().take_while(|t| punct(t, "(")).count();
    let Some(first) = part.get(at) else { return Ok(()) };
    if first.0 != Tok::Word {
        return Err(format!("This statement is not allowed on a read-only connection. {REFUSED}"));
    }
    let rest = &part[at + 1..];
    let verb = first.1.to_ascii_uppercase();
    match verb.as_str() {
        "SELECT" | "TABLE" | "VALUES" | "SHOW" | "HELP" | "USE" => Ok(()),
        "WITH" => check_with(rest),
        "DESC" | "DESCRIBE" | "EXPLAIN" => match rest.first() {
            // Only ANALYZE runs the statement it explains.
            Some(t) if is(t, "ANALYZE") => {
                let inner = &rest[1..];
                let inner = if inner.first().is_some_and(|t| is(t, "FORMAT")) { inner.get(3..).unwrap_or(&[]) } else { inner };
                if inner.is_empty() { Err(format!("EXPLAIN ANALYZE needs a statement to run. {REFUSED}")) } else { check_one(inner) }
            }
            _ => Ok(()),
        },
        "SET" => {
            // Every comma separated assignment outside parentheses must start with @name.
            let mut depth = 0i32;
            let mut starts = vec![0];
            for (i, t) in rest.iter().enumerate() {
                if punct(t, "(") {
                    depth += 1;
                } else if punct(t, ")") {
                    depth -= 1;
                } else if depth == 0 && punct(t, ",") {
                    starts.push(i + 1);
                }
            }
            let user_var = |i: &usize| rest.get(*i).is_some_and(|t| t.0 == Tok::Word && t.1.starts_with('@') && !t.1.starts_with("@@"));
            if rest.is_empty() || !starts.iter().all(user_var) {
                return Err("Only SET @variable is allowed on a read-only connection.".into());
            }
            Ok(())
        }
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "TRUNCATE" | "LOAD" | "CREATE" | "ALTER" | "DROP" | "RENAME" | "GRANT" | "REVOKE" => Err(format!("{verb} writes data and this connection is read-only. {REFUSED}")),
        _ => Err(format!("{verb} is not allowed on a read-only connection. {REFUSED}")),
    }
}

/// MySQL 8 lets a CTE feed INSERT, UPDATE, DELETE or REPLACE, so look for them outside parentheses.
fn check_with(rest: &[Code]) -> Result<(), String> {
    let mut depth = 0i32;
    for (i, t) in rest.iter().enumerate() {
        if punct(t, "(") {
            depth += 1;
        } else if punct(t, ")") {
            depth -= 1;
        } else if depth == 0 && t.0 == Tok::Word {
            let w = t.1.to_ascii_uppercase();
            let called = rest.get(i + 1).is_some_and(|n| punct(n, "("));
            let locking = i > 0 && is(&rest[i - 1], "FOR");
            let write = match w.as_str() {
                "INSERT" | "REPLACE" => !called,
                "UPDATE" => !locking,
                "DELETE" => true,
                _ => false,
            };
            if write {
                return Err(format!("{w} inside WITH writes data and this connection is read-only. {REFUSED}"));
            }
        }
    }
    Ok(())
}

/// One piece of a script: a statement to send to the server, or a client side `DELIMITER` line that only
/// changes where the next statements end. A statement is bytes, not text: a dump may hold binary values
/// written raw inside quotes, and those must reach the server unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Stmt(Vec<u8>),
    Delimiter(String),
}

/// Where the scan of the pending statement stopped. Kept between feeds, so a statement that arrives in many
/// chunks is read once instead of from its start on every chunk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum At {
    /// Before the first code of a piece: whitespace and comments, after which a DELIMITER line may still come.
    Lead,
    Code,
    Quote(u8),
    Line,
    Block,
}

/// Splits a script that arrives in chunks into statements, the way the mysql client reads a dump: it
/// understands `DELIMITER`, so a procedure body with its own `;` stays one statement. Quotes, backticks and
/// comments hide a delimiter. A `/*! ... */` comment is kept whole, since the server runs what is inside it.
/// A statement is never cut by a chunk edge, so a dump of any size needs only its longest statement in memory.
/// Every delimiter, quote and comment mark is ASCII, so it works on bytes and never needs the file to be UTF-8.
pub struct ScriptSplitter {
    buf: Vec<u8>,
    /// Start of the pending piece.
    pos: usize,
    /// The next byte to look at; `at` and `code` say what the bytes before it were.
    scan: usize,
    at: At,
    /// The pending piece holds code. A piece of only comments is not sent: the server would answer "query was empty".
    code: bool,
    delim: Vec<u8>,
    backslash: bool,
    eof: bool,
    /// Bytes dropped from the front of `buf`, so `offset` keeps counting from the start of the script.
    dropped: u64,
    /// The longest statement the server accepts. Without it an unclosed quote would read the whole file into memory.
    max: usize,
    #[cfg(test)]
    seen: u64,
}

fn trim_end(b: &[u8]) -> &[u8] {
    &b[..b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(0, |p| p + 1)]
}

impl ScriptSplitter {
    /// `backslash` is false when the session runs with NO_BACKSLASH_ESCAPES.
    pub fn new(backslash: bool) -> ScriptSplitter {
        ScriptSplitter { buf: Vec::new(), pos: 0, scan: 0, at: At::Lead, code: false, delim: b";".to_vec(), backslash, eof: false, dropped: 0, max: usize::MAX, #[cfg(test)] seen: 0 }
    }

    /// A SET of sql_mode inside the script changes how the server reads the text that follows.
    pub fn set_backslash(&mut self, on: bool) {
        self.backslash = on;
    }

    /// The server's max_allowed_packet: a pending statement past it is an error rather than more memory.
    pub fn set_max(&mut self, max: usize) {
        self.max = max.max(1);
    }

    /// Bytes of the script read so far, for a progress bar.
    pub fn offset(&self) -> u64 {
        self.dropped + self.pos as u64
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.dropped += self.pos as u64;
            self.scan -= self.pos;
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// No more text follows, so a last statement without its delimiter is complete.
    pub fn finish(&mut self) {
        self.eof = true;
    }

    /// The next piece, or None when it needs more text (or the script is over). A statement is only returned
    /// once its delimiter was seen inside the text fed so far, so what comes next cannot change how it reads.
    pub fn next_piece(&mut self) -> Result<Option<Piece>, String> {
        #[cfg(test)]
        let from = self.scan;
        let r = self.step();
        #[cfg(test)]
        {
            let reached = if matches!(r, Ok(None)) { self.buf.len() } else { self.scan };
            self.seen += reached.saturating_sub(from) as u64;
        }
        r
    }

    /// Stops at `i`, which the next call reads again once more text arrived.
    fn wait(&mut self, i: usize) -> Result<Option<Piece>, String> {
        self.scan = i;
        if self.buf.len() - self.pos > self.max {
            return Err(format!("A statement is longer than {} bytes, the server's max_allowed_packet, so it cannot be sent. A quote or comment left open earlier in the file reads the same way.", self.max));
        }
        Ok(None)
    }

    /// The pending piece up to `end`; the next one starts at `next`.
    fn take(&mut self, end: usize, next: usize) -> Vec<u8> {
        let text = trim_end(&self.buf[self.pos..end]).to_vec();
        (self.pos, self.scan, self.at, self.code) = (next, next, At::Lead, false);
        text
    }

    fn step(&mut self) -> Result<Option<Piece>, String> {
        let n = self.buf.len();
        let mut i = self.scan;
        loop {
            if i >= n {
                if !self.eof {
                    return self.wait(n);
                }
                let code = self.code;
                let text = self.take(n, n);
                return Ok(code.then_some(Piece::Stmt(text)));
            }
            let b = &self.buf;
            let after = if self.code { At::Code } else { At::Lead };
            // Up to nine bytes decide a mark, so a chunk edge inside one waits for more.
            let eof = self.eof;
            let need = |i: usize, k: usize| i + k >= n && !eof;
            match self.at {
                At::Quote(q) => {
                    let esc = self.backslash && q != b'`';
                    match b[i..].iter().position(|&c| c == q || (esc && c == b'\\')) {
                        None => i = n,
                        Some(p) => {
                            i += p;
                            // An escaped or a doubled quote does not close the string, so the next byte decides.
                            if need(i, 1) {
                                return self.wait(i);
                            }
                            if b[i] == b'\\' || b.get(i + 1) == Some(&q) {
                                i += 2;
                            } else {
                                i += 1;
                                self.at = At::Code;
                            }
                        }
                    }
                }
                At::Line => match b[i..].iter().position(|&c| c == b'\n') {
                    None => i = n,
                    Some(p) => {
                        i += p;
                        self.at = after;
                    }
                },
                At::Block => match b[i..].windows(2).position(|w| w == b"*/") {
                    // A `*` at the very end may be the first half of the close.
                    None if !self.eof && b[n - 1] == b'*' => return self.wait(n - 1),
                    None => i = n,
                    Some(p) => {
                        i += p + 2;
                        self.at = after;
                    }
                },
                At::Lead | At::Code => {
                    let d = self.delim.len();
                    if b[i..].starts_with(&self.delim) {
                        let code = self.code;
                        let text = self.take(i, i + d);
                        if code {
                            return Ok(Some(Piece::Stmt(text)));
                        }
                        i = self.scan;
                        continue;
                    }
                    if n - i < d && !self.eof && self.delim.starts_with(&b[i..]) {
                        return self.wait(i);
                    }
                    let c = b[i];
                    let lead = self.at == At::Lead;
                    if c == b'\'' || c == b'"' || c == b'`' {
                        (self.at, self.code) = (At::Quote(c), true);
                        i += 1;
                    } else if c == b'#' {
                        self.at = At::Line;
                        i += 1;
                    } else if c == b'-' {
                        if need(i, 1) || (b.get(i + 1) == Some(&b'-') && need(i, 2)) {
                            return self.wait(i);
                        }
                        if b.get(i + 1) == Some(&b'-') && b.get(i + 2).is_none_or(|x| x.is_ascii_whitespace() || x.is_ascii_control()) {
                            self.at = At::Line;
                            i += 2;
                        } else {
                            (self.at, self.code) = (At::Code, true);
                            i += 1;
                        }
                    } else if c == b'/' {
                        if need(i, 1) || (b.get(i + 1) == Some(&b'*') && (need(i, 2) || (b.get(i + 2) == Some(&b'M') && need(i, 3)))) {
                            return self.wait(i);
                        }
                        if b.get(i + 1) == Some(&b'*') {
                            // `/*!` and MariaDB's `/*M!` hold code, so a comment of only those counts as a statement.
                            let exec = b.get(i + 2) == Some(&b'!') || (b.get(i + 2) == Some(&b'M') && b.get(i + 3) == Some(&b'!'));
                            self.code |= exec;
                            self.at = At::Block;
                            i += 2;
                        } else {
                            (self.at, self.code) = (At::Code, true);
                            i += 1;
                        }
                    } else if c.is_ascii_whitespace() {
                        // Whitespace before a piece is not part of it.
                        if lead && i == self.pos {
                            self.pos += 1;
                        }
                        i += 1;
                    } else if lead && self.dropped == 0 && i == 0 && c == 0xEF {
                        // A byte order mark at the start of the file is not text.
                        if need(i, 2) {
                            return self.wait(i);
                        }
                        if b.starts_with(b"\xEF\xBB\xBF") {
                            (self.pos, i) = (3, 3);
                        } else {
                            (self.at, self.code) = (At::Code, true);
                            i += 1;
                        }
                    } else if lead && (c == b'd' || c == b'D') {
                        // A DELIMITER line, which may follow comments (phpMyAdmin writes a header before it).
                        if need(i, 9) {
                            return self.wait(i);
                        }
                        let word = n >= i + 9 && b[i..i + 9].eq_ignore_ascii_case(b"delimiter") && b.get(i + 9).is_none_or(|x| x.is_ascii_whitespace());
                        if !word {
                            (self.at, self.code) = (At::Code, true);
                            i += 1;
                            continue;
                        }
                        let Some(nl) = b[i + 9..].iter().position(|&x| x == b'\n').map(|p| i + 9 + p).or(self.eof.then_some(n)) else { return self.wait(i) };
                        let arg = b[i + 9..nl].split(|x| x.is_ascii_whitespace()).find(|w| !w.is_empty()).unwrap_or_default();
                        if arg.is_empty() {
                            return Err("DELIMITER needs the text that ends a statement.".into());
                        }
                        if arg.contains(&b'\\') {
                            return Err("A delimiter cannot contain a backslash.".into());
                        }
                        let arg = String::from_utf8_lossy(arg).into_owned();
                        let next = (nl + 1).min(n);
                        self.take(next, next);
                        self.delim = arg.clone().into_bytes();
                        return Ok(Some(Piece::Delimiter(arg)));
                    } else {
                        (self.at, self.code) = (At::Code, true);
                        i += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn use_names_its_database() {
        assert_eq!(use_target("USE shop").as_deref(), Some("shop"));
        assert_eq!(use_target("  use `my``db` ").as_deref(), Some("my`db"));
        assert_eq!(use_target("/* c */ USE x").as_deref(), Some("x"));
        assert_eq!(use_target("SELECT 1"), None);
        assert_eq!(use_target("USER_THING"), None);
        assert_eq!(use_target("USE 'q'"), None);
    }

    use super::*;

    // The MySQL tests below keep their one-argument calls through these.
    const D: Dialect = Dialect::MySql;
    fn tokens(t: &str) -> Vec<(Tok, Range<usize>)> {
        super::tokens(t, D)
    }
    fn split_statements(t: &str) -> Vec<(Range<usize>, &str)> {
        super::split_statements(t, D)
    }
    fn statement_at(t: &str, c: usize) -> Option<Range<usize>> {
        super::statement_at(t, c, D)
    }
    fn read_only_check(t: &str) -> Result<(), String> {
        super::read_only_check(t, D)
    }
    fn uses_prepared(t: &str) -> bool {
        super::uses_prepared(t, D)
    }
    fn changes_escaping(t: &str) -> bool {
        super::changes_escaping(t, D)
    }
    fn count_with(t: &str, bs: bool, ansi: bool) -> usize {
        super::count_with(t, D, bs, ansi)
    }
    fn placeholders(t: &str, bs: bool, ansi: bool) -> (Vec<usize>, usize) {
        super::placeholders(t, D, bs, ansi)
    }
    fn pageable(t: &str) -> bool {
        super::pageable(t, D)
    }
    fn has_order_by(t: &str) -> bool {
        super::has_order_by(t, D)
    }
    fn use_target(t: &str) -> Option<String> {
        super::use_target(t, D)
    }

    #[test]
    fn the_head_of_a_statement_reads_as_the_whole_would() {
        let pg = super::Dialect::Postgres;
        assert_eq!(super::head_words("  -- note\n/* c */ commit  and chain; x", pg, 3), ["COMMIT", "AND", "CHAIN"]);
        assert_eq!(super::head_words("begin", pg, 3), ["BEGIN"]);
        // A long statement is read from its start only, and one whose start has too few words is still read whole.
        let big = format!("INSERT INTO t VALUES ({})", "'x', ".repeat(5000) + "1");
        assert_eq!(super::head_words(&big, pg, 3), ["INSERT", "INTO", "T"]);
        let late = format!("{}SELECT 1", "/* x */ ".repeat(1000));
        assert_eq!(super::head_words(&late, pg, 2), ["SELECT", "1"]);
        // A multibyte character at the cut does not split.
        let wide = format!("{} commit", "é".repeat(3000));
        assert_eq!(super::head_words(&wide, pg, 2).len(), 2);
        assert!(!super::needs_no_transaction(&big, pg) && !super::transaction_control(&big, pg));
        assert!(super::needs_no_transaction("VACUUM t", pg) && super::transaction_control("rollback", pg));
    }

    fn texts(s: &str) -> Vec<&str> {
        split_statements(s).into_iter().map(|(_, t)| t).collect()
    }

    #[test]
    fn splits_outside_strings_and_comments() {
        assert_eq!(texts("SELECT 1; SELECT 2;"), ["SELECT 1", "SELECT 2"]);
        assert_eq!(texts("SELECT 'a;b', \"c;d\", `e;f`; SELECT 2"), ["SELECT 'a;b', \"c;d\", `e;f`", "SELECT 2"]);
        assert_eq!(texts("SELECT 1 -- x; y\n; SELECT 2 # a;b\n; SELECT /* ; */ 3"), ["SELECT 1", "SELECT 2", "SELECT /* ; */ 3"]);
        assert_eq!(texts(";; ;\n-- only a comment\n;/* c */;"), Vec::<&str>::new());
    }

    #[test]
    fn quotes_escape_in_both_ways() {
        assert_eq!(texts(r"SELECT 'it\'s;' ; SELECT 2"), [r"SELECT 'it\'s;'", "SELECT 2"]);
        assert_eq!(texts("SELECT 'it''s;'; SELECT 2"), ["SELECT 'it''s;'", "SELECT 2"]);
        assert_eq!(texts("SELECT `a``;b`; SELECT 2"), ["SELECT `a``;b`", "SELECT 2"]);
        // A backslash does not escape inside backticks.
        assert_eq!(texts(r"SELECT `a\`; SELECT 2"), [r"SELECT `a\`", "SELECT 2"]);
        assert_eq!(texts("SELECT 'open;"), ["SELECT 'open;"]);
    }

    #[test]
    fn double_dash_needs_a_space() {
        assert_eq!(texts("SELECT 1--1; SELECT 2"), ["SELECT 1--1", "SELECT 2"]);
        assert_eq!(texts("SELECT 1 --; SELECT 2"), ["SELECT 1 --", "SELECT 2"]);
        assert_eq!(texts("SELECT 1 -- ; SELECT 2"), ["SELECT 1"]);
        assert_eq!(texts("SELECT 1 --\t; SELECT 2"), ["SELECT 1"]);
        assert_eq!(texts("SELECT 1 --"), ["SELECT 1"]);
    }

    #[test]
    fn executable_comments_are_code() {
        assert_eq!(texts("SELECT 1 /*! ; DELETE FROM t */"), ["SELECT 1 /*!", "DELETE FROM t */"]);
        assert_eq!(texts("/*!80000 SELECT 1 */; SELECT 2"), ["/*!80000 SELECT 1 */", "SELECT 2"]);
        assert_eq!(texts("/*! */"), Vec::<&str>::new());
        let t = tokens("/*!80000 x */");
        assert_eq!(t[0].0, Tok::ExecOpen);
        assert_eq!(t.last().unwrap().0, Tok::ExecClose);
        // Without the bang it is an ordinary comment.
        assert_eq!(tokens("/* x */ y").iter().filter(|(t, _)| *t == Tok::Comment).count(), 1);
    }

    #[test]
    fn ranges_find_the_statement_under_the_caret() {
        let s = "SELECT 1;  SELECT 2;\nSELECT 3";
        let r = split_statements(s);
        assert_eq!(r[1].0, 11..19);
        assert_eq!(statement_at(s, 0), Some(0..8));
        assert_eq!(statement_at(s, 9), Some(0..8));
        assert_eq!(statement_at(s, 10), Some(11..19));
        assert_eq!(statement_at(s, 15), Some(11..19));
        assert_eq!(statement_at(s, 21), Some(21..29));
        assert_eq!(statement_at(s, 999), Some(21..29));
        assert_eq!(statement_at("  ;  ", 1), None);
    }

    #[test]
    fn non_ascii_text_keeps_char_boundaries() {
        let s = "SELECT 'é;ü', é; SELECT 2";
        assert_eq!(texts(s), ["SELECT 'é;ü', é", "SELECT 2"]);
    }

    fn refused(s: &str) -> String {
        read_only_check(s).expect_err(s)
    }

    #[test]
    fn reading_statements_pass() {
        for s in [
            "SELECT 1",
            "select * from t where a = 'DELETE'",
            "(SELECT 1) UNION (SELECT 2)",
            "SHOW TABLES",
            "DESC t",
            "DESCRIBE t",
            "EXPLAIN SELECT 1",
            "EXPLAIN UPDATE t SET a = 1",
            "TABLE t",
            "VALUES ROW(1)",
            "HELP 'x'",
            "USE shop",
            "SET @x = 1",
            "set @a := 1, @b = 2",
            "SET @x = IF(a, b, c)",
            "SELECT 1 -- DELETE FROM t",
            "SELECT 1 /* DELETE FROM t */",
            "SELECT * FROM t FOR UPDATE",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "WITH c AS (SELECT 1) SELECT REPLACE(a, 'b', 'c'), INSERT(a, 1, 1, 'x') FROM c",
            "WITH c AS (SELECT 1) SELECT * FROM c FOR UPDATE",
            "SELECT 1; SELECT 2",
            "",
            "  -- nothing",
            "/*!80000 SELECT 1 */",
            "EXPLAIN ANALYZE SELECT 1",
            "EXPLAIN ANALYZE FORMAT=TREE SELECT 1",
            "SELECT 'INTO OUTFILE'",
            "SELECT `into` outfile FROM t",
        ] {
            assert_eq!(read_only_check(s), Ok(()), "{s}");
        }
    }

    #[test]
    fn writing_statements_are_refused_with_a_plain_message() {
        assert_eq!(refused("UPDATE t SET a = 1"), "UPDATE writes data and this connection is read-only. Unlock writes first.");
        assert!(refused("delete from t").starts_with("DELETE writes data"));
        assert!(refused("INSERT INTO t VALUES (1)").starts_with("INSERT writes"));
        assert!(refused("DROP TABLE t").starts_with("DROP writes"));
        assert!(refused("CALL p()").starts_with("CALL is not allowed"));
        assert!(refused("ANALYZE TABLE t").starts_with("ANALYZE is not allowed"));
        assert!(refused("-- c\nDELETE FROM t").starts_with("DELETE"));
        assert!(refused("SELECT 1; DELETE FROM t").starts_with("DELETE"));
    }

    #[test]
    fn set_only_allows_user_variables() {
        for s in [
            "SET SESSION transaction_read_only = 0",
            "SET GLOBAL x = 1",
            "SET @@session.sql_mode = ''",
            "SET @@global.x = 1",
            "SET TRANSACTION READ WRITE",
            "SET NAMES utf8mb4",
            "SET autocommit = 1",
            "SET @x = 1, @@sql_mode = ''",
            "SET @x = 1, GLOBAL y = 2",
            "SET",
            "SET STATEMENT max_statement_time=1 FOR DELETE FROM t",
        ] {
            assert_eq!(refused(s), "Only SET @variable is allowed on a read-only connection.", "{s}");
        }
    }

    #[test]
    fn cte_with_a_top_level_write_is_refused() {
        assert!(refused("WITH c AS (SELECT 1) DELETE FROM t WHERE a IN (SELECT * FROM c)").contains("DELETE inside WITH"));
        assert!(refused("WITH c AS (SELECT 1) UPDATE t SET a = 1").contains("UPDATE inside WITH"));
        assert!(refused("WITH c AS (SELECT 1) INSERT INTO t SELECT * FROM c").contains("INSERT inside WITH"));
        assert!(refused("WITH c AS (SELECT 1) REPLACE INTO t SELECT * FROM c").contains("REPLACE inside WITH"));
    }

    #[test]
    fn explain_analyze_runs_its_statement() {
        assert!(refused("EXPLAIN ANALYZE DELETE FROM t").starts_with("DELETE writes"));
        assert!(refused("EXPLAIN ANALYZE FORMAT=TREE UPDATE t SET a = 1").starts_with("UPDATE writes"));
        assert!(refused("EXPLAIN ANALYZE").contains("needs a statement"));
        assert!(refused("DESCRIBE ANALYZE WITH c AS (SELECT 1) DELETE FROM t").contains("inside WITH"));
    }

    #[test]
    fn outfile_is_refused_anywhere() {
        assert!(refused("SELECT * FROM t INTO OUTFILE '/tmp/x'").contains("OUTFILE"));
        assert!(refused("select 1 into\n dumpfile '/tmp/x'").contains("OUTFILE"));
        assert!(refused("TABLE t INTO OUTFILE '/tmp/x'").contains("OUTFILE"));
        assert!(refused("SELECT 1 /*! INTO OUTFILE '/tmp/x' */").contains("OUTFILE"));
    }

    #[test]
    fn executable_comments_are_checked_as_code() {
        assert!(refused("SELECT 1 /*! ; DELETE FROM t */").starts_with("DELETE"));
        assert!(refused("/*!DELETE FROM t*/").starts_with("DELETE"));
        assert!(refused("/*!50000 DROP TABLE t */").starts_with("DROP"));
        assert!(refused("/*M!100100 DELETE FROM t */").starts_with("DELETE"));
        assert!(refused("SELECT 1 /*!80000 INTO OUTFILE 'x' */").contains("OUTFILE"));
        assert_eq!(read_only_check("SELECT 1 /*!80000 , 2 */"), Ok(()));
    }

    #[test]
    fn versioned_comments_are_checked_both_ways() {
        for s in [
            "/*!99999 SELECT */ DROP DATABASE shop",
            "/*M! SELECT */ DROP TABLE t",
            "/*M!999999 SELECT */ DROP TABLE t",
            "SELECT 1 INTO /*!99999 x */ OUTFILE '/tmp/f'",
            "WITH c AS (SELECT 1) /*!99999 ( */ DELETE FROM t",
            "SET @x = /*!99999 ( */ 1, GLOBAL read_only = 0",
        ] {
            assert!(read_only_check(s).is_err(), "{s}");
        }
    }

    #[test]
    fn quote_modes_cannot_hide_a_statement() {
        assert!(read_only_check(r"SELECT 'a\'; DROP TABLE t; -- '").is_err());
        assert!(read_only_check(r#"SELECT "a\"; DROP TABLE t; -- ""#).is_err());
        for s in [r"SELECT 'it\'s'", r#"SELECT "a" FROM t"#, "SELECT 'a''b'", r"SELECT 'C:\\' , 1"] {
            assert_eq!(read_only_check(s), Ok(()), "{s}");
        }
    }

    #[test]
    fn user_variable_needs_no_gap() {
        assert!(read_only_check("SET @/**/@sql_mode = ''").is_err());
        assert!(read_only_check("SET @ x = 1").is_err());
        assert_eq!(read_only_check("SET @`a b` = 1, @'c' = 2, @x := 3"), Ok(()));
        assert!(read_only_check("SET @@sql_mode = ''").is_err());
    }

    #[test]
    fn sequence_changes_are_refused() {
        for s in ["SELECT NEXTVAL(s)", "SELECT SETVAL(s, 10)", "SELECT NEXT VALUE FOR s", "SELECT s.nextval", "SELECT nextval (s)"] {
            assert!(refused(s).contains("sequence"), "{s}");
        }
        assert_eq!(read_only_check("SELECT LASTVAL(s), PREVIOUS VALUE FOR s"), Ok(()));
        assert_eq!(read_only_check("SELECT nextval FROM t"), Ok(()));
    }

    #[test]
    fn ranges_stop_at_the_last_code_token() {
        let s = "SELECT 1 -- note\n;\nSELECT 2  /* c */  \n";
        assert_eq!(texts(s), ["SELECT 1", "SELECT 2"]);
        assert_eq!(statement_at(s, 12), Some(0..8));
        assert_eq!(statement_at(s, 30), Some(19..27));
        assert_eq!(statement_at(s, 33), Some(19..27));
    }

    #[test]
    fn escaping_changes_are_found() {
        for s in ["SET NAMES utf8", "set names 'utf8'", "SET CHARACTER SET utf8", "SET CHARSET utf8", "SET SESSION character_set_client = 1", "SET @@character_set_connection=1", "SET sql_mode=''", "SET @@sql_mode = ''", "SET @@session.sql_mode = ''", "SET @a = 1, `sql_mode` = ''", "SELECT 1; SET NAMES utf8", "SET /*!50000 NAMES */ utf8"] {
            assert!(changes_escaping(s), "{s}");
        }
        for s in ["SET @sql_mode = 1", "SET @a = 'sql_mode'", "SET @a = (SELECT names FROM t)", "SELECT 'SET NAMES x'", "SET autocommit = 0"] {
            assert!(!changes_escaping(s), "{s}");
        }
    }

    #[test]
    fn prepared_statements_are_found() {
        for s in ["PREPARE s FROM 'SET sql_mode=\'\''", "execute s", "DEALLOCATE PREPARE s", "SELECT 1; EXECUTE s USING @a", "/*!50000 PREPARE */ s FROM 'x'"] {
            assert!(uses_prepared(s), "{s}");
        }
        for s in ["SELECT 'PREPARE x'", "SELECT execute FROM t", "SET @a = 1", "SELECT 1 -- EXECUTE s"] {
            assert!(!uses_prepared(s), "{s}");
        }
    }

    #[test]
    fn readings_differ_on_a_backslash_in_quotes() {
        let t = "SELECT '\\'' AS a, ? AS b -- '";
        assert_ne!(placeholders(t, true, false), placeholders(t, false, false));
        assert_eq!(placeholders("SELECT ?, '?'", true, false), placeholders("SELECT ?, '?'", false, false));
        assert_eq!(count_with("SELECT '\\''; SELECT 2", false, false), 1);
        assert_eq!(count_with("SELECT '\\''; SELECT 2", true, false), 2);
    }

    #[test]
    fn merge_stops_paging_on_postgres_only() {
        // MERGE is a keyword of PostgreSQL's DML, and an ordinary name on MySQL.
        assert!(super::pageable("SELECT merge FROM t", Dialect::MySql));
        assert!(!super::pageable("SELECT merge FROM t", Dialect::Postgres));
        assert!(!super::pageable("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE", Dialect::Postgres));
        assert!(!super::pageable("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE", Dialect::MySql));
    }

    #[test]
    fn only_plain_reads_are_paged() {
        for ok in ["SELECT 1", "select * from t order by a", "WITH c AS (SELECT 1) SELECT * FROM c", "TABLE t", "VALUES ROW(1), ROW(2)", "SELECT (SELECT 1 INTO @x) FROM t"] {
            assert!(pageable(ok), "{ok}");
        }
        for no in ["SHOW TABLES", "EXPLAIN SELECT 1", "CALL p()", "SELECT 1 INTO @x", "SELECT * FROM t FOR UPDATE", "SELECT * FROM t LOCK IN SHARE MODE", "WITH c AS (SELECT 1) DELETE FROM t", "SELECT /*!50000 1 */", "UPDATE t SET a = 1", ""] {
            assert!(!pageable(no), "{no}");
        }
        assert!(has_order_by("SELECT * FROM t ORDER BY a") && !has_order_by("SELECT * FROM (SELECT 1 ORDER BY 1) x") && !has_order_by("SELECT 'order by'"));
    }

    #[test]
    fn the_wrapped_page_is_still_a_read_and_survives_a_trailing_comment() {
        let w = page_sql("SELECT * FROM t ORDER BY a -- last\n;", 2, 1000);
        assert_eq!(w, "SELECT * FROM (\nSELECT * FROM t ORDER BY a -- last\n) AS q LIMIT 1001 OFFSET 2000");
        assert!(read_only_check(&w).is_ok() && read_only_check(&count_sql("SELECT 1")).is_ok());
        assert_eq!(count_sql("SELECT 1;"), "SELECT COUNT(*) FROM (\nSELECT 1\n) AS q");
        // A statement that is refused stays refused inside the wrapper.
        assert!(read_only_check(&page_sql("SELECT 1 INTO OUTFILE '/tmp/x'", 0, 1000)).is_err());
    }

    /// Every piece of `bytes` when they arrive `chunk` bytes at a time, cut anywhere, inside characters too.
    fn pieces_b(bytes: &[u8], chunk: usize, bs: bool) -> Result<Vec<Piece>, String> {
        let mut sp = ScriptSplitter::new(bs);
        let mut out = Vec::new();
        for c in bytes.chunks(chunk.max(1)) {
            sp.feed(c);
            while let Some(p) = sp.next_piece()? {
                out.push(p);
            }
        }
        sp.finish();
        while let Some(p) = sp.next_piece()? {
            out.push(p);
        }
        Ok(out)
    }

    fn pieces(text: &str, chunk: usize, bs: bool) -> Result<Vec<Piece>, String> {
        pieces_b(text.as_bytes(), chunk, bs)
    }

    fn stmts(text: &str) -> Vec<String> {
        // Chunk sizes that cut inside quotes, comments, delimiters and characters must all agree.
        let whole = pieces(text, text.len().max(1), true).unwrap();
        for chunk in [1, 2, 3, 5, 7, 16] {
            assert_eq!(pieces(text, chunk, true).unwrap(), whole, "chunk {chunk} of {text:?}");
        }
        whole.into_iter().map(|p| match p { Piece::Stmt(s) => String::from_utf8(s).unwrap(), Piece::Delimiter(d) => format!("DELIMITER {d}") }).collect()
    }

    #[test]
    fn a_script_splits_on_semicolons_outside_quotes_and_comments() {
        assert_eq!(stmts("SELECT 1; SELECT ';'; SELECT \"a;b\"; SELECT `x;y`"), ["SELECT 1", "SELECT ';'", "SELECT \"a;b\"", "SELECT `x;y`"]);
        assert_eq!(stmts("SELECT 1 -- ; not here\n; # nor ;\nSELECT 2; /* ; */ SELECT 3"), ["SELECT 1 -- ; not here", "# nor ;\nSELECT 2", "/* ; */ SELECT 3"]);
        // A comment alone is not a statement, the server would answer "query was empty".
        assert_eq!(stmts("/* a */; -- b\n; SELECT 1;"), ["SELECT 1"]);
        assert!(stmts("   \n ; ;; \n").is_empty());
    }

    #[test]
    fn delimiter_lines_change_where_statements_end() {
        let dump = "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND$$\nDELIMITER ;\nSELECT 3;\n";
        assert_eq!(stmts(dump), ["DELIMITER $$", "CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND", "DELIMITER ;", "SELECT 3"]);
        let dump = "delimiter //\nCREATE TRIGGER t BEFORE INSERT ON a FOR EACH ROW BEGIN SET NEW.x = 1; END//\nDELIMITER ;\n";
        assert_eq!(stmts(dump), ["DELIMITER //", "CREATE TRIGGER t BEFORE INSERT ON a FOR EACH ROW BEGIN SET NEW.x = 1; END", "DELIMITER ;"]);
        // Two characters long, and `;;` as mysqldump writes it.
        assert_eq!(stmts("DELIMITER ;;\nSELECT 1;SELECT 2;;\nDELIMITER ;\nSELECT 3;"), ["DELIMITER ;;", "SELECT 1;SELECT 2", "DELIMITER ;", "SELECT 3"]);
    }

    #[test]
    fn a_delimiter_inside_a_string_or_comment_does_not_end_the_statement() {
        let dump = "DELIMITER $$\nSELECT '$$', \"$$\", `a$$b` $$\nSELECT 1 /* $$ */ -- $$\n, 2 $$\nDELIMITER ;\n";
        assert_eq!(stmts(dump), ["DELIMITER $$", "SELECT '$$', \"$$\", `a$$b`", "SELECT 1 /* $$ */ -- $$\n, 2", "DELIMITER ;"]);
        // A value that tries to end the statement stays inside its quotes, escaped or doubled.
        assert_eq!(stmts("INSERT INTO t VALUES ('a\\'; DROP TABLE t; --'); SELECT 1"), ["INSERT INTO t VALUES ('a\\'; DROP TABLE t; --')", "SELECT 1"]);
        assert_eq!(stmts("INSERT INTO t VALUES ('a''; DROP TABLE t; --'); SELECT 1"), ["INSERT INTO t VALUES ('a''; DROP TABLE t; --')", "SELECT 1"]);
    }

    #[test]
    fn backslashes_follow_the_servers_sql_mode() {
        // Under NO_BACKSLASH_ESCAPES the backslash is a plain character, so this quote ends the string.
        let text = "SELECT 'a\\'; SELECT 2";
        let normal = pieces(text, 100, true).unwrap();
        assert_eq!(normal, [Piece::Stmt("SELECT 'a\\'; SELECT 2".into())]);
        let plain = pieces(text, 100, false).unwrap();
        assert_eq!(plain, [Piece::Stmt("SELECT 'a\\'".into()), Piece::Stmt("SELECT 2".into())]);
    }

    #[test]
    fn executable_comments_stay_in_one_piece() {
        let dump = "/*!40101 SET @OLD=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO' */;\n/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`%`*/ /*!50003 TRIGGER t BEFORE INSERT ON a FOR EACH ROW SET NEW.x = 1 */;;\n/*M!100100 SET x=1 */;";
        let got = stmts(dump);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].starts_with("/*!40101 SET @OLD") && got[0].ends_with("*/"));
        assert!(got[1].contains("TRIGGER t BEFORE INSERT"));
        assert!(stmts("/*!40101 SET x=1 */;").len() == 1);
    }

    #[test]
    fn a_last_statement_needs_no_delimiter_and_a_bad_delimiter_line_is_an_error() {
        assert_eq!(stmts("SELECT 1; SELECT 2"), ["SELECT 1", "SELECT 2"]);
        assert_eq!(stmts("DELIMITER //\nSELECT 1; SELECT 2"), ["DELIMITER //", "SELECT 1; SELECT 2"]);
        assert!(pieces("DELIMITER a\\b\nSELECT 1", 100, true).is_err());
        // The word as a column or table name is just a statement.
        assert_eq!(stmts("SELECT delimiter FROM t; delimiters;"), ["SELECT delimiter FROM t", "delimiters"]);
    }

    #[test]
    fn progress_counts_the_bytes_that_were_turned_into_pieces() {
        let text = "SELECT 1;\nSELECT 2;\n";
        let mut sp = ScriptSplitter::new(true);
        sp.feed(text.as_bytes());
        sp.finish();
        assert_eq!(sp.next_piece().unwrap(), Some(Piece::Stmt("SELECT 1".into())));
        assert_eq!(sp.offset(), 9);
        assert_eq!(sp.next_piece().unwrap(), Some(Piece::Stmt("SELECT 2".into())));
        assert_eq!(sp.next_piece().unwrap(), None);
        assert_eq!(sp.offset(), text.len() as u64);
    }

    #[test]
    fn multibyte_text_survives_a_chunk_cut() {
        assert_eq!(stmts("SELECT 'héllo; wörld ✓'; SELECT 'ü'"), ["SELECT 'héllo; wörld ✓'", "SELECT 'ü'"]);
    }

    #[test]
    fn a_delimiter_line_after_comments_is_still_a_delimiter_line() {
        // phpMyAdmin and hand written files put a comment header before DELIMITER.
        let body = "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END";
        for head in ["-- Procedures\n", "--\n-- Procedures\n--\n", "/* header */\n", "# note\n", "/* a */ -- b\n# c\n  "] {
            let text = format!("{head}DELIMITER $$\n{body}$$\nDELIMITER ;\nSELECT 3;\n");
            assert_eq!(stmts(&text), ["DELIMITER $$", body, "DELIMITER ;", "SELECT 3"], "{head:?}");
        }
        // Comments before a statement stay in its text.
        assert_eq!(stmts("-- note\nSELECT 1;\n/* c */ SELECT 2;"), ["-- note\nSELECT 1", "/* c */ SELECT 2"]);
        // An executable comment is code, so a DELIMITER after it is not a client line.
        assert_eq!(stmts("/*!40101 SET x=1 */ delimiter ;"), ["/*!40101 SET x=1 */ delimiter"]);
    }

    #[test]
    fn a_bare_delimiter_line_is_an_error_and_never_sent() {
        for text in ["DELIMITER\n", "SELECT 1;\nDELIMITER", "DELIMITER   \nSELECT 1;", "-- c\nDELIMITER\r\nSELECT 1;"] {
            for chunk in [1, 3, 100] {
                let r = pieces(text, chunk, true);
                assert!(r.as_ref().is_err_and(|e| e.contains("DELIMITER needs")), "{text:?} chunk {chunk}: {r:?}");
            }
        }
    }

    #[test]
    fn raw_bytes_reach_the_statement_unchanged() {
        // A mysqldump BLOB written without --hex-blob: bytes that are not UTF-8 between escapes.
        let first = b"INSERT INTO t VALUES (1, '\\0\xFF\x80\xC3\xE2A\\n\\''), (2, _binary '\xC3;\xE2')".to_vec();
        let text = [&first[..], b";\nSELECT 'caf\xE9';"].concat();
        let want = vec![Piece::Stmt(first), Piece::Stmt(b"SELECT 'caf\xE9'".to_vec())];
        for chunk in [1, 2, 3, 5, 7, text.len()] {
            assert_eq!(pieces_b(&text, chunk, true).unwrap(), want, "chunk {chunk}");
        }
        // A byte order mark is not part of the first statement, cut or not.
        for chunk in [1, 2, 100] {
            assert_eq!(pieces_b(b"\xEF\xBB\xBFSELECT 1;", chunk, true).unwrap(), [Piece::Stmt(b"SELECT 1".to_vec())]);
        }
    }

    #[test]
    fn a_long_statement_in_many_chunks_is_read_once() {
        let mut stmt = String::from("INSERT INTO t VALUES ");
        while stmt.len() < 32 << 20 {
            stmt.push_str("('abcdefghijklmnopqrstuvwxyz0123456789',12345),('it''s \\'q\\'', \"d\"),");
        }
        stmt.push_str("(1,1);\n");
        let mut sp = ScriptSplitter::new(true);
        let mut got = Vec::new();
        for c in stmt.as_bytes().chunks(256 << 10) {
            sp.feed(c);
            while let Some(p) = sp.next_piece().unwrap() {
                got.push(p);
            }
        }
        sp.finish();
        while let Some(p) = sp.next_piece().unwrap() {
            got.push(p);
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], Piece::Stmt(stmt.trim_end().trim_end_matches(';').as_bytes().to_vec()));
        // Each byte is looked at about once, not once per chunk that came after it.
        assert!(sp.seen < 2 * stmt.len() as u64, "{} bytes looked at for {}", sp.seen, stmt.len());
    }

    #[test]
    fn a_statement_past_max_allowed_packet_is_an_error_not_more_memory() {
        let mut sp = ScriptSplitter::new(true);
        sp.set_max(1000);
        sp.feed(b"SELECT 1; SELECT 'unclosed ");
        assert_eq!(sp.next_piece().unwrap(), Some(Piece::Stmt(b"SELECT 1".to_vec())));
        let mut err = None;
        for _ in 0..10 {
            sp.feed(&[b'x'; 300]);
            if let Err(e) = sp.next_piece() {
                err = Some(e);
                break;
            }
        }
        assert!(err.is_some_and(|e| e.contains("max_allowed_packet")));
        // A statement under the limit is fine, however many of them follow.
        let mut sp = ScriptSplitter::new(true);
        sp.set_max(1000);
        for _ in 0..100 {
            sp.feed(b"SELECT 'a fairly short statement';\n");
            while sp.next_piece().unwrap().is_some() {}
        }
    }
}
