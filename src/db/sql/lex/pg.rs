//! The PostgreSQL side of the lexer: how its text is tokenised and what the read-only check refuses.
//! This is the first of three layers (the session default and a rolled back READ ONLY transaction are the
//! others), so it refuses whatever it cannot read with certainty.

use super::{is, punct, quote_end, Code, Tok, REFUSED};
use std::ops::Range;

fn ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn ident_cont(c: u8) -> bool {
    ident_start(c) || c.is_ascii_digit() || c == b'$'
}

/// Splits PostgreSQL text into tokens that cover it completely. `plain_backslash` is the reading where
/// standard_conforming_strings is off and a plain '...' honours backslashes; E'...' always does. Unterminated
/// strings and comments run to the end of the text, as they are a syntax error on the server anyway.
pub(super) fn lex(text: &str, plain_backslash: bool) -> Vec<(Tok, Range<usize>)> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let s = i;
        let c = b[i];
        let next = b.get(i + 1).copied();
        let tok = if c.is_ascii_whitespace() || c == 0x0b {
            while i < n && (b[i].is_ascii_whitespace() || b[i] == 0x0b) {
                i += 1;
            }
            Tok::Space
        } else if c == b'\'' {
            i = quote_end(b, i, plain_backslash);
            Tok::Str
        } else if c == b'"' {
            i = quote_end(b, i, false);
            Tok::Quoted
        } else if c == b'-' && next == Some(b'-') {
            // A carriage return ends a comment on the server as well, and text after it is code.
            while i < n && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
            Tok::Comment
        } else if c == b'/' && next == Some(b'*') {
            // Block comments nest, so an inner */ does not end the outer one.
            let (mut depth, mut j) = (1, i + 2);
            while j < n && depth > 0 {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            i = j;
            Tok::Comment
        } else if c == b'$' {
            // $1 is a parameter. $tag$ or $$ opens a string that only the same mark closes. A `$` glued to a
            // word never gets here because the word swallows it.
            let mut j = i + 1;
            if next.is_some_and(|d| d.is_ascii_digit()) {
                while j < n && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
                Tok::Word
            } else {
                // The tag follows identifier rules except that it cannot hold a `$`.
                if j < n && ident_start(b[j]) {
                    while j < n && (ident_start(b[j]) || b[j].is_ascii_digit()) {
                        j += 1;
                    }
                }
                if j < n && b[j] == b'$' {
                    let mark = &text[i..=j];
                    i = text[j + 1..].find(mark).map_or(n, |p| j + 1 + p + mark.len());
                    Tok::Str
                } else {
                    i += 1;
                    Tok::Punct
                }
            }
        } else if ident_start(c) {
            while i < n && ident_cont(b[i]) {
                i += 1;
            }
            let one = i - s == 1;
            let q = b.get(i).copied();
            if one && q == Some(b'\'') && matches!(c, b'e' | b'E' | b'b' | b'B' | b'x' | b'X') {
                // E'' honours backslashes in every reading; B'' and X'' never do.
                i = quote_end(b, i, matches!(c, b'e' | b'E'));
                Tok::Str
            } else if one && matches!(c, b'u' | b'U') && q == Some(b'&') && matches!(b.get(i + 1), Some(b'\'' | b'"')) {
                let quote = b[i + 1];
                i = quote_end(b, i + 1, false);
                if quote == b'"' { Tok::Quoted } else { Tok::Str }
            } else {
                Tok::Word
            }
        } else if c.is_ascii_digit() {
            // A `$` inside a number-led word reads differently before and after PostgreSQL 16, so the whole run
            // is one word here and the read-only check refuses it.
            while i < n && ident_cont(b[i]) {
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

/// Whether a word token is `$1`, `$2`, ... and which number.
/// The dollar quote for a body: `$base$` with an x added until the body has no `$base` in it, so the body cannot hold its own
/// closing tag. `function` is the tag `pg_get_functiondef` picks, which a function body of a trigger edit has to match.
pub(in crate::db::sql) fn dollar_quote(body: &str, base: &str) -> String {
    let mut t = format!("${base}");
    while body.contains(&t) {
        t.push('x');
    }
    t.push('$');
    t
}

pub(super) fn param_number(w: &str) -> Option<usize> {
    let d = w.strip_prefix('$')?;
    if d.is_empty() || !d.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    d.parse().ok()
}

/// The name an identifier token spells, folded to lower case. A Unicode escape name cannot be decoded here
/// (the escape character may be changed), so it comes back as a marker no list contains.
const UNSURE: &str = "\0unicode escape";

fn ident(t: &Code) -> Option<String> {
    match t.0 {
        Tok::Word => Some(t.1.to_ascii_lowercase()),
        Tok::Quoted if t.1.len() >= 2 && t.1.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("u&")) => Some(UNSURE.into()),
        Tok::Quoted => Some(t.1.trim_start_matches('"').trim_end_matches('"').replace("\"\"", "\"").to_ascii_lowercase()),
        _ => None,
    }
}

/// Exact names of functions that change state or reach outside the database. A READ ONLY transaction stops many
/// of them but not all (set_config on a session, advisory locks, sequences, pg_cancel_backend), and a lexer that
/// lets one through would be the only guard on a connection whose session default someone turned off. These
/// match after a `.` as well, because `(expr).f` and `alias.f` call `f(expr)`.
pub(super) const REFUSED_FUNCTIONS_EXACT: &[&str] = &[
    // Settings, sessions and server control.
    "set_config", "pg_terminate_backend", "pg_cancel_backend", "pg_reload_conf", "pg_rotate_logfile", "pg_switch_wal", "pg_create_restore_point", "pg_promote",
    "pg_logical_emit_message", "pg_stat_statements_reset", "pg_start_backup", "pg_stop_backup", "pg_log_backend_memory_contexts", "pg_log_standby_snapshot",
    "pg_import_system_collations", "pg_notify", "pg_export_snapshot", "setseed",
    // Sequences and transaction ids, which a READ ONLY transaction does not always stop.
    "nextval", "setval", "txid_current", "pg_current_xact_id",
    // Server side files: reads of arbitrary paths and the listings of server directories.
    "pg_read_file", "pg_read_binary_file", "pg_stat_file", "pg_ls_dir", "pg_ls_logdir", "pg_ls_waldir", "pg_ls_tmpdir", "pg_ls_archive_statusdir", "pg_ls_logicalsnapdir",
    "pg_ls_logicalmapdir", "pg_ls_replslotdir", "pg_ls_summariesdir", "pg_file_write", "pg_file_rename", "pg_file_unlink", "pg_file_sync", "pg_logfile_rotate",
    // Large objects: they write to pg_largeobject and read server files.
    "lo_import", "lo_export", "lo_unlink", "lo_create", "lo_creat", "lo_open", "lo_put", "lo_get", "lo_truncate", "lo_truncate64", "lo_from_bytea", "lo_close", "lo_lseek", "lo_lseek64",
    "lo_tell", "lo_tell64", "lowrite", "loread",
    // Maintenance that rewrites data, statistics or on-disk state.
    "pg_prewarm", "brin_summarize_new_values", "brin_summarize_range", "brin_desummarize_range", "gin_clean_pending_list", "pg_restore_relation_stats",
    "pg_restore_attribute_stats", "pg_clear_relation_stats", "pg_clear_attribute_stats", "heap_force_kill", "heap_force_freeze", "pg_truncate_visibility_map",
    "pg_buffercache_evict", "pg_buffercache_evict_relation", "pg_buffercache_evict_all", "autoprewarm_dump_now", "autoprewarm_start_worker", "pg_nextoid",
    // Replication slots and origins: they create or drop server objects. The view pg_replication_slots must stay readable, so the families are listed by name.
    "pg_create_physical_replication_slot", "pg_create_logical_replication_slot", "pg_drop_replication_slot", "pg_copy_physical_replication_slot", "pg_copy_logical_replication_slot",
    "pg_replication_slot_advance", "pg_sync_replication_slots", "pg_alter_replication_slot", "pg_replication_origin_create", "pg_replication_origin_drop", "pg_replication_origin_oid",
    "pg_replication_origin_session_setup", "pg_replication_origin_session_reset", "pg_replication_origin_session_is_setup", "pg_replication_origin_session_progress",
    "pg_replication_origin_xact_setup", "pg_replication_origin_xact_reset", "pg_replication_origin_advance", "pg_replication_origin_progress",
    // These run the SQL text they are given, which this check never sees.
    "cursor_to_xml", "xpath_table", "ts_stat", "ts_rewrite", "crosstab", "crosstab2", "crosstab3", "crosstab4", "connectby",
    // pg_background runs SQL in another session that this transaction does not cover.
    "pg_background_launch", "pg_background_result", "pg_background_detach",
    // Citus runs this text on worker nodes over connections of its own.
    "master_run_on_worker",
];

/// Functions that run SQL text the check never reads, so the settings that text changes cannot be seen either.
const RUNS_SQL_TEXT: &[&str] = &["cursor_to_xml", "xpath_table", "ts_stat", "ts_rewrite", "crosstab", "crosstab2", "crosstab3", "crosstab4", "connectby", "pg_background_launch", "master_run_on_worker"];

/// Families matched on the start of the name, in a call and after a `.`. Only families where no column or
/// relation name is likely to start the same way.
pub(super) const REFUSED_FUNCTIONS_PREFIX: &[&str] = &[
    // Advisory locks, dblink connections and statements, and replay or backup control.
    "pg_advisory", "pg_try_advisory", "pg_logical_slot_", "dblink", "pg_stat_reset", "pg_backup_", "pg_wal_replay_",
    // These run the SQL text they are given, on this or another server.
    "query_to_xml", "run_command_on_",
];

/// Families matched on the start of the name only when the name is called. They are not matched after a `.`
/// because ordinary catalog names start the same way (pg_catalog.pg_file_settings, pg_catalog.pg_replication_origin_status,
/// t.lo_price), and the exact list above names the functions of each family for the `.` case.
pub(super) const REFUSED_FUNCTIONS_CALL_PREFIX: &[&str] = &["lo_", "pg_file_", "pg_replication_origin_"];

/// Names matched anywhere inside, in a call only, for the same reason as above (pg_replication_slots is a view).
pub(super) const REFUSED_FUNCTIONS_CONTAIN: &[&str] = &["replication_slot"];

/// Whether the name is refused as the function of a call.
pub(super) fn refused_function(name: &str) -> bool {
    refused_field(name) || REFUSED_FUNCTIONS_CALL_PREFIX.iter().any(|p| name.starts_with(p)) || REFUSED_FUNCTIONS_CONTAIN.iter().any(|p| name.contains(p))
}

/// Whether the name is refused after a `.`, where it may be a column as well as a function.
fn refused_field(name: &str) -> bool {
    REFUSED_FUNCTIONS_EXACT.contains(&name) || REFUSED_FUNCTIONS_PREFIX.iter().any(|p| name.starts_with(p))
}

/// Whether the token at `i` is the name of a call (`f(`) or follows a `.`. `(expr).f` and `alias.f` call f(expr), so a name
/// after a `.` is as much a call as a name before `(`.
fn call_or_field(part: &[Code], i: usize) -> (bool, bool) {
    (part.get(i + 1).is_some_and(|n| punct(n, "(")), i > 0 && punct(&part[i - 1], "."))
}

/// Built-in functions a read may call but a CHECK may not: the server runs a CHECK on every row written, so a sleep there stalls
/// every insert and update of the table while it holds its locks.
const CHECK_REFUSED_PREFIX: &[&str] = &["pg_sleep"];

/// The first function in an expression that the read-only check refuses, under both readings of backslashes, for text that
/// runs later on every row (a CHECK constraint), where such a call would act on each insert. Only names of built-ins are known
/// here; the create reads the volatility of every other function the CHECK reaches once the server resolved it.
pub(crate) fn refused_call(expr: &str) -> Option<String> {
    [false, true].into_iter().find_map(|bs| {
        let code = code_of(expr, bs);
        (0..code.len()).find_map(|i| {
            let (called, field) = call_or_field(&code, i);
            let name = ident(&code[i]).filter(|_| called || field)?;
            if name == UNSURE {
                return Some("a function named with a Unicode escape".to_string());
            }
            let check_only = CHECK_REFUSED_PREFIX.iter().any(|p| name.starts_with(p));
            ((called && refused_function(&name)) || (field && refused_field(&name)) || check_only).then_some(name)
        })
    })
}

/// Every function in the text that the read-only check refuses, under both readings of backslashes, for a caller that lets
/// some of them through and must still see the rest.
pub(crate) fn refused_calls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for bs in [false, true] {
        let code = code_of(text, bs);
        for i in 0..code.len() {
            let (called, field) = call_or_field(&code, i);
            let Some(name) = ident(&code[i]).filter(|_| called || field) else { continue };
            let hit = name == UNSURE || (called && refused_function(&name)) || (field && refused_field(&name));
            if hit && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Functions whose effect stays on the session after the transaction ends: a session advisory lock stays on the backend
/// and a dblink connection stays open there. The transaction-scoped `pg_advisory_xact_lock` family is not listed.
const SESSION_FUNCTIONS_EXACT: &[&str] = &["pg_advisory_lock", "pg_advisory_lock_shared", "pg_try_advisory_lock", "pg_try_advisory_lock_shared"];
const SESSION_FUNCTIONS_PREFIX: &[&str] = &["dblink"];

/// Whether the statement calls a function that leaves session state behind, in any spelling the read-only check reads
/// (plain, schema-qualified, quoted, field notation), under both readings of backslashes in strings.
pub(super) fn session_function(stmt: &str) -> bool {
    [false, true].into_iter().any(|bs| {
        let code = code_of(stmt, bs);
        (0..code.len()).any(|i| {
            let (called, field) = call_or_field(&code, i);
            (called || field) && ident(&code[i]).is_some_and(|n| SESSION_FUNCTIONS_EXACT.contains(&n.as_str()) || SESSION_FUNCTIONS_PREFIX.iter().any(|p| n.starts_with(p)))
        })
    })
}

/// Statements that read, besides EXPLAIN which is checked for ANALYZE and then for its inner statement.
const READS: [&str; 5] = ["SELECT", "WITH", "VALUES", "TABLE", "SHOW"];

fn code_of(stmt: &str, plain_backslash: bool) -> Vec<Code<'_>> {
    lex(stmt, plain_backslash).into_iter().filter(|(t, _)| matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct)).map(|(t, r)| (t, &stmt[r])).collect()
}

/// Allow-list on the first keyword, then a scan of the whole statement for locking clauses, INTO, data
/// modifying CTEs and the functions above. The text must pass with standard_conforming_strings on and off.
pub(super) fn read_only_check(stmt: &str) -> Result<(), String> {
    for plain_backslash in [false, true] {
        let code = code_of(stmt, plain_backslash);
        if code.iter().any(|t| t.0 == Tok::Word && t.1.as_bytes()[0].is_ascii_digit() && t.1.contains('$')) {
            return Err(format!("A number followed by $ reads differently on different servers, so this statement is not allowed on a read-only connection. {REFUSED}"));
        }
        code.split(|t| punct(t, ";")).try_for_each(check_one)?;
    }
    Ok(())
}

fn check_one(part: &[Code]) -> Result<(), String> {
    // psql commands start with a backslash, and \g or \gexec can follow a statement on the same line.
    if part.iter().any(|t| punct(t, "\\")) {
        return Err(format!("psql backslash commands are not allowed on a read-only connection. {REFUSED}"));
    }
    // A statement may open with parentheses, as in (SELECT 1) UNION (SELECT 2).
    let at = part.iter().take_while(|t| punct(t, "(")).count();
    let Some(first) = part.get(at) else { return Ok(()) };
    if first.0 != Tok::Word {
        return Err(format!("This statement is not allowed on a read-only connection. {REFUSED}"));
    }
    let verb = first.1.to_ascii_uppercase();
    if verb == "EXPLAIN" {
        return check_explain(&part[at + 1..]);
    }
    if !READS.contains(&verb.as_str()) {
        return Err(match verb.as_str() {
            "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "TRUNCATE" | "CREATE" | "ALTER" | "DROP" | "GRANT" | "REVOKE" | "COMMENT" | "REASSIGN" | "IMPORT" | "SECURITY" => format!("{verb} writes data and this connection is read-only. {REFUSED}"),
            _ => format!("{verb} is not allowed on a read-only connection. {REFUSED}"),
        });
    }
    check_body(part)
}

/// ANALYZE runs the statement it explains, in either spelling and in the option list too.
fn check_explain(rest: &[Code]) -> Result<(), String> {
    let analyze = |t: &Code| ident(t).is_some_and(|w| w == "analyze" || w == "analyse" || w == UNSURE);
    let mut j = 0;
    if rest.first().is_some_and(|t| punct(t, "(")) {
        let mut depth = 0i32;
        let mut close = None;
        for (k, t) in rest.iter().enumerate() {
            if punct(t, "(") {
                depth += 1;
            } else if punct(t, ")") {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
        }
        let Some(close) = close else { return Err(format!("EXPLAIN options are not closed. {REFUSED}")) };
        // Only `ANALYZE` followed by one plain false word is a plan without execution. Anything else that
        // mentions it, including a value this cannot read, is refused.
        let (mut depth, mut start) = (0i32, 1);
        for k in 1..=close {
            if punct(&rest[k], "(") {
                depth += 1;
            } else if punct(&rest[k], ")") && k < close {
                depth -= 1;
            }
            if k == close || (depth == 0 && punct(&rest[k], ",")) {
                let opt = &rest[start..k];
                start = k + 1;
                if opt.iter().any(analyze) {
                    let off = opt.len() == 2 && opt[1].0 == Tok::Word && ["false", "off", "0"].iter().any(|v| opt[1].1.eq_ignore_ascii_case(v)) && ident(&opt[0]).is_some_and(|w| w == "analyze" || w == "analyse");
                    if !off {
                        return Err(format!("EXPLAIN ANALYZE runs the statement it explains. {REFUSED}"));
                    }
                }
            }
        }
        j = close + 1;
    } else {
        while rest.get(j).is_some_and(|t| t.0 == Tok::Word && (analyze(t) || t.1.eq_ignore_ascii_case("verbose"))) {
            if analyze(&rest[j]) {
                return Err(format!("EXPLAIN ANALYZE runs the statement it explains. {REFUSED}"));
            }
            j += 1;
        }
    }
    let inner = &rest[j..];
    if inner.is_empty() {
        return Err(format!("EXPLAIN needs a statement to explain. {REFUSED}"));
    }
    check_one(inner)
}

/// What no statement that reads may hold, wherever it sits.
fn check_body(part: &[Code]) -> Result<(), String> {
    for (i, t) in part.iter().enumerate() {
        let next = part.get(i + 1);
        if t.0 == Tok::Word {
            let w = t.1.to_ascii_uppercase();
            if w == "INTO" {
                return Err(format!("SELECT INTO creates a table and is not allowed on a read-only connection. {REFUSED}"));
            }
            if w == "FOR" && next.is_some_and(|n| n.0 == Tok::Word && ["UPDATE", "SHARE", "NO", "KEY"].contains(&n.1.to_ascii_uppercase().as_str())) {
                return Err(format!("A locking clause (FOR UPDATE, FOR SHARE) takes row locks and is not allowed on a read-only connection. {REFUSED}"));
            }
        }
        let (called, field) = call_or_field(part, i);
        if (called || field) && let Some(name) = ident(t) {
            if name == UNSURE {
                return Err(format!("A function named with a Unicode escape cannot be checked and is not allowed on a read-only connection. {REFUSED}"));
            }
            if (called && refused_function(&name)) || (field && refused_field(&name)) {
                return Err(format!("{name}() changes state and is not allowed on a read-only connection. {REFUSED}"));
            }
        }
    }
    // A data modifying statement can only sit in a WITH, but a column or alias may share its name, so look
    // only when a WITH is there.
    if part.iter().any(|t| is(t, "WITH")) && let Some(t) = part.iter().find(|t| ["INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|k| is(t, k))) {
        return Err(format!("{} inside WITH writes data and this connection is read-only. {REFUSED}", t.1.to_ascii_uppercase()));
    }
    Ok(())
}

/// Whether the text prepares or runs a statement from a name or a string.
pub(super) fn uses_prepared(stmt: &str) -> bool {
    [false, true].into_iter().any(|bs| code_of(stmt, bs).split(|t| punct(t, ";")).any(|part| part.first().is_some_and(|t| is(t, "PREPARE") || is(t, "EXECUTE") || is(t, "DEALLOCATE"))))
}

/// Settings that decide how text is read or whether a session may write. Quarry escapes values for what it
/// read at connect and sets the search path itself, so a script must not move any of them.
const ESCAPING_SETTINGS: [&str; 10] = [
    "standard_conforming_strings", "backslash_quote", "escape_string_warning", "client_encoding", "search_path", "default_transaction_read_only",
    "transaction_read_only", "tx_read_only", "session_authorization", "role",
];

/// Words that stand for one of the settings in SET and RESET: SET NAMES, SET SCHEMA, SET SESSION AUTHORIZATION,
/// SET SESSION CHARACTERISTICS, SET TRANSACTION READ WRITE and RESET ALL.
const ESCAPING_WORDS: [&str; 6] = ["names", "schema", "authorization", "characteristics", "write", "all"];

pub(super) fn changes_escaping(stmt: &str) -> bool {
    [false, true].into_iter().any(|bs| {
        let code = code_of(stmt, bs);
        code.split(|t| punct(t, ";")).any(|part| {
            let first = part.iter().find(|t| !punct(t, "("));
            // A DO block is code the check cannot read, and set_config takes the setting name as text.
            if first.is_some_and(|t| is(t, "DO") || is(t, "DISCARD")) {
                return true;
            }
            // A Unicode-escaped name can spell any setting or table, and SQL-running functions hide their text.
            let hidden = |w: &str| w == UNSURE || RUNS_SQL_TEXT.contains(&w) || ["query_to_xml", "run_command_on_", "dblink"].iter().any(|p| w.starts_with(p));
            if part.iter().filter_map(ident).any(|w| hidden(&w)) {
                return true;
            }
            if part.iter().enumerate().any(|(i, t)| part.get(i + 1).is_some_and(|n| punct(n, "(")) && ident(t).is_some_and(|w| w == "set_config")) {
                return true;
            }
            // UPDATE pg_settings changes a setting the way SET does, and a view or rule made over it does the same
            // through another name, so any statement naming it that is not a plain read counts.
            let reads = first.is_some_and(|t| ["SELECT", "TABLE", "VALUES", "SHOW", "EXPLAIN"].iter().any(|w| is(t, w)));
            if part.iter().any(|t| ident(t).is_some_and(|w| w == "pg_settings")) && (!reads || part.iter().any(|t| is(t, "UPDATE"))) {
                return true;
            }
            if !first.is_some_and(|t| is(t, "SET") || is(t, "RESET")) {
                return false;
            }
            part[1..].iter().filter_map(ident).any(|w| ESCAPING_SETTINGS.contains(&w.as_str()) || ESCAPING_WORDS.contains(&w.as_str()) || w == UNSURE)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::super::{changes_escaping, read_only_check, split_statements, tokens, uses_prepared, Dialect};
    use super::*;

    const PG: Dialect = Dialect::Postgres;

    fn ro(s: &str) -> Result<(), String> {
        read_only_check(s, PG)
    }

    fn kinds(s: &str) -> Vec<(Tok, &str)> {
        tokens(s, PG).into_iter().filter(|(t, _)| *t != Tok::Space).map(|(t, r)| (t, &s[r])).collect()
    }

    #[test]
    fn strings_comments_and_names_end_where_postgres_ends_them() {
        use Tok::*;
        // `$` glued to a word is part of the word, so no dollar quote opens.
        assert_eq!(kinds("a$b$c"), [(Word, "a$b$c")]);
        assert_eq!(kinds("SELECT $$a;b$$"), [(Word, "SELECT"), (Str, "$$a;b$$")]);
        assert_eq!(kinds("$tag$ $$ ; $tag$ x"), [(Str, "$tag$ $$ ; $tag$"), (Word, "x")]);
        // Tags are case sensitive and a different tag does not close.
        assert_eq!(kinds("$A$ x $a$ y $A$ z"), [(Str, "$A$ x $a$ y $A$"), (Word, "z")]);
        assert_eq!(kinds("$a$ x $b$ y"), [(Str, "$a$ x $b$ y")]);
        assert_eq!(kinds("$1 + $22"), [(Word, "$1"), (Punct, "+"), (Word, "$22")]);
        assert_eq!(kinds("$1$$ x $$"), [(Word, "$1"), (Str, "$$ x $$")]);
        // A tag cannot start with a digit, so this is a parameter and a word.
        assert_eq!(kinds("$1a$"), [(Word, "$1"), (Word, "a$")]);
        assert_eq!(kinds(r"E'a\'b' 'c\' x"), [(Str, r"E'a\'b'"), (Str, r"'c\'"), (Word, "x")]);
        assert_eq!(kinds(r"U&'a\' B'01' X'1f' n'x'"), [(Str, r"U&'a\'"), (Str, "B'01'"), (Str, "X'1f'"), (Word, "n"), (Str, "'x'")]);
        assert_eq!(kinds(r#"U&"a""b" "c""d" `e`"#), [(Quoted, r#"U&"a""b""#), (Quoted, r#""c""d""#), (Punct, "`"), (Word, "e"), (Punct, "`")]);
        // A word before the quote means the letter is part of a name, not a prefix.
        assert_eq!(kinds("xE'a'"), [(Word, "xE"), (Str, "'a'")]);
        assert_eq!(kinds("a::int"), [(Word, "a"), (Punct, ":"), (Punct, ":"), (Word, "int")]);
        assert_eq!(kinds("1 --c\n2 -- d\n3 #4"), [(Word, "1"), (Comment, "--c"), (Word, "2"), (Comment, "-- d"), (Word, "3"), (Punct, "#"), (Word, "4")]);
        // A carriage return ends a line comment.
        assert_eq!(kinds("1 --c\r2"), [(Word, "1"), (Comment, "--c"), (Word, "2")]);
        assert_eq!(kinds("/* a /* b */ c */ d"), [(Comment, "/* a /* b */ c */"), (Word, "d")]);
        assert_eq!(kinds("/*! x */ y"), [(Comment, "/*! x */"), (Word, "y")]);
        assert_eq!(kinds("/* open"), [(Comment, "/* open")]);
        // Plain strings take backslashes only in the reading where standard_conforming_strings is off.
        assert_eq!(super::lex(r"'a\' b", true).len(), 1);
        assert_eq!(super::lex(r"'a\' b", false).len(), 3);
        assert_eq!(split_statements("SELECT $$;$$; SELECT 2", PG).len(), 2);
    }

    const WRITES: &[&str] = &[
        "SET x = 1", "SET LOCAL x TO 1", "SET SESSION x = 1", "RESET ALL", "RESET x", "BEGIN", "START TRANSACTION", "COMMIT", "END", "ROLLBACK", "SAVEPOINT a", "COPY t TO '/tmp/x'", "COPY t FROM PROGRAM 'id'",
        "DO $$ BEGIN END $$", "CALL p()", "SELECT 1 INTO t2", "SELECT * INTO TEMP t2 FROM t", "LISTEN c", "NOTIFY c", "UNLISTEN *", "PREPARE p AS SELECT 1", "EXECUTE p", "DEALLOCATE p",
        "DECLARE c CURSOR FOR SELECT 1", "FETCH 1 FROM c", "MOVE 1 IN c", "LOCK TABLE t", "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE", "VACUUM", "ANALYZE", "ANALYSE", "CLUSTER t", "REINDEX TABLE t",
        "REFRESH MATERIALIZED VIEW m", "CHECKPOINT", "DISCARD ALL", "LOAD 'x'", r"\dt", r"\gexec", r"SELECT 1 \gexec", "INSERT INTO t VALUES (1)", "UPDATE t SET a = 1", "DELETE FROM t", "TRUNCATE t",
        "CREATE TABLE t (a int)", "ALTER TABLE t ADD c int", "DROP TABLE t", "GRANT ALL ON t TO u", "REVOKE ALL ON t FROM u", "COMMENT ON TABLE t IS 'x'", "CREATE TABLE t AS SELECT 1", "SECURITY LABEL ON TABLE t IS 'x'",
        "SELECT * FROM t FOR UPDATE", "SELECT * FROM t FOR SHARE", "SELECT * FROM t FOR NO KEY UPDATE", "SELECT * FROM t FOR KEY SHARE", "select * from t for update skip locked", "(SELECT 1) UNION (SELECT 2 FOR UPDATE)",
        "SELECT * FROM (SELECT * FROM t FOR UPDATE) s", "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x", "WITH x AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM x",
        "WITH x AS (UPDATE t SET a = 1 RETURNING *) SELECT * FROM x", "WITH x AS (MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE RETURNING *) SELECT * FROM x",
        "WITH a AS (SELECT 1), b AS (WITH c AS (DELETE FROM t RETURNING 1) SELECT * FROM c) SELECT * FROM b", "WITH RECURSIVE x AS (SELECT 1 UNION ALL SELECT 2) INSERT INTO t SELECT * FROM x",
        "EXPLAIN ANALYZE SELECT 1", "explain analyse select 1", "EXPLAIN (ANALYZE) SELECT 1", "EXPLAIN (ANALYZE true, VERBOSE) SELECT 1", "EXPLAIN (ANALYZE on) SELECT 1", "EXPLAIN (ANALYZE 1) SELECT 1", "EXPLAIN (ANALYZE false, ANALYZE true) SELECT 1", "EXPLAIN (ANALYZE false, VERBOSE, ANALYZE) SELECT 1", "EXPLAIN (ANALYZE (false)) SELECT 1", "EXPLAIN (ANALYZE 'false') SELECT 1", "EXPLAIN (ANALYZE false off) SELECT 1", "EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1", "EXPLAIN (FORMAT JSON, analyze) SELECT 1",
        r#"EXPLAIN ("analyze") SELECT 1"#, "EXPLAIN ANALYZE VERBOSE SELECT 1", "EXPLAIN VERBOSE ANALYZE SELECT 1", "EXPLAIN INSERT INTO t VALUES (1)", "EXPLAIN (FORMAT JSON) DELETE FROM t", "EXPLAIN", "EXPLAIN (VERBOSE",
        "EXPLAIN (ANALYZE) WITH x AS (DELETE FROM t RETURNING 1) SELECT * FROM x", "SELECT 1; DROP TABLE t", "SELECT 1; SET x = 1", "SELECT 1;\nCOMMIT", "SELECT 1 -- c\n; DROP TABLE t", "\"select\" 1", "1",
        // A number with a `$` inside reads differently across server versions, so it hides nothing.
        "SELECT 1$$ ; DROP TABLE t; $$", "SELECT 1e5$a$ ; DROP TABLE t; $a$",
    ];

    #[test]
    fn every_write_is_refused() {
        for w in WRITES {
            assert!(ro(w).is_err(), "{w}");
        }
        // Nothing runs for an empty text.
        assert!(ro("").is_ok() && ro(" ; ;").is_ok());
    }

    const FUNCTIONS: &[&str] = &[
        "set_config", "pg_terminate_backend", "pg_cancel_backend", "pg_reload_conf", "pg_rotate_logfile", "pg_switch_wal", "pg_create_restore_point", "pg_promote", "pg_advisory_lock", "pg_advisory_xact_lock_shared",
        "pg_try_advisory_lock", "pg_try_advisory_xact_lock", "pg_logical_slot_get_changes", "pg_logical_slot_get_binary_changes", "dblink", "dblink_exec", "dblink_connect", "lo_import", "lo_export", "lo_unlink", "lo_create", "lo_put", "lowrite", "loread", "pg_file_write", "pg_file_unlink",
        "pg_logical_emit_message", "pg_create_logical_replication_slot", "pg_drop_replication_slot", "pg_replication_slot_advance", "pg_replication_origin_create", "pg_replication_origin_advance", "pg_stat_reset",
        "pg_stat_reset_shared", "pg_stat_reset_single_table_counters", "pg_stat_statements_reset", "pg_backup_start", "pg_backup_stop", "pg_start_backup", "pg_stop_backup", "pg_wal_replay_pause", "pg_wal_replay_resume",
        "pg_log_backend_memory_contexts", "pg_import_system_collations", "pg_notify", "nextval", "setval", "pg_read_file", "pg_read_binary_file", "pg_ls_dir", "txid_current", "pg_current_xact_id", "query_to_xml",
        "query_to_xml_and_xmlschema", "cursor_to_xml", "xpath_table", "ts_stat", "ts_rewrite", "crosstab", "crosstab2", "crosstab3", "crosstab4", "connectby", "pg_background_launch", "pg_background_result", "pg_background_detach",
        "run_command_on_workers", "run_command_on_shards", "run_command_on_placements", "run_command_on_colocated_placements", "run_command_on_all_nodes", "heap_force_kill", "heap_force_freeze", "pg_truncate_visibility_map",
        "pg_buffercache_evict", "pg_buffercache_evict_relation", "pg_buffercache_evict_all", "setseed", "pg_stat_file", "pg_ls_logdir", "pg_ls_waldir", "pg_ls_tmpdir", "pg_ls_archive_statusdir", "pg_ls_logicalsnapdir",
        "pg_ls_logicalmapdir", "pg_ls_replslotdir", "pg_ls_summariesdir", "pg_create_physical_replication_slot", "pg_copy_physical_replication_slot", "pg_copy_logical_replication_slot", "pg_export_snapshot",
        "pg_sync_replication_slots", "pg_replication_origin_drop", "pg_replication_origin_session_setup", "pg_replication_origin_xact_setup", "pg_file_rename", "pg_file_sync", "lo_creat", "lo_open", "lo_get", "lo_truncate", "lo_truncate64",
        "lo_from_bytea", "lo_close", "lo_lseek", "lo_lseek64", "lo_tell", "lo_tell64",
    ];

    /// Every way a call to `f` may be written, as statements.
    fn spellings(f: &str) -> Vec<String> {
        let up = f.to_ascii_uppercase();
        vec![
            format!("SELECT {f}('a')"),
            format!("select pg_catalog.{f}('a')"),
            format!("SELECT \"{f}\"('a')"),
            format!("SELECT \"pg_catalog\".\"{f}\"('a')"),
            format!("SELECT {up}('a')"),
            format!("SELECT \"{up}\"('a')"),
            format!("SELECT Pg_Catalog.{up}('a')"),
            format!("SELECT {f} /* c */ ('a')"),
            format!("SELECT {f} /* a /* b */ c */ ('a')"),
            format!("SELECT {f} -- c\n('a')"),
            format!("SELECT {f}\n\t ('a')"),
            format!("SELECT {f}('a'), 1"),
            format!("SELECT * FROM {f}('a')"),
            format!("SELECT 1 WHERE {f}('a') IS NULL"),
            format!("WITH x AS (SELECT {f}('a')) SELECT * FROM x"),
            format!("SELECT * FROM (SELECT {f}('a')) s"),
            format!("EXPLAIN SELECT {f}('a')"),
            format!("EXPLAIN (VERBOSE) SELECT {f}('a')"),
            format!("(SELECT {f}('a'))"),
            format!("SELECT 1; SELECT {f}('a')"),
            format!("VALUES ({f}('a'))"),
            format!("SELECT 'x' || {f}('a')"),
        ]
    }

    #[test]
    fn every_refused_function_is_refused_in_every_spelling() {
        for f in FUNCTIONS {
            for s in spellings(f) {
                assert!(ro(&s).is_err(), "{s}");
                // The same text after a string that only ends under standard_conforming_strings on.
                assert!(ro(&format!("SELECT 'a\\' ; {s}")).is_err(), "{s}");
            }
        }
        // A Unicode escape can spell any name, and its escape character can change, so none is accepted.
        for s in [r#"SELECT U&"set\005fconfig"('a', 'b', false)"#, r#"SELECT U&"set!005fconfig"('a', 'b', false) UESCAPE '!'"#, r#"select u&"x"(1)"#] {
            assert!(ro(s).is_err(), "{s}");
        }
        // Names that only start like a refused one, or are not called, stay usable.
        for s in ["SELECT nextval_x(1)", "SELECT my_set_config(1)", "SELECT setval FROM t", "SELECT lo FROM t", "SELECT lower(a) FROM t", "SELECT pg_advisory FROM t", "SELECT \"set_config\" FROM t", "SELECT set_config$(1)", "SELECT dbl FROM t"] {
            assert!(ro(s).is_ok(), "{s}");
        }
    }

    #[test]
    fn dollar_quotes_hide_keywords_only_when_they_are_quotes() {
        for s in [
            "SELECT $$ ; DROP TABLE t; $$",
            "SELECT $a$ ; DROP TABLE t; $a$",
            "SELECT $a$ x $b$; DROP TABLE t; $a$",
            "SELECT $a$ x $$ ; DROP TABLE t; $$ y $a$",
            "SELECT $é$ ; DROP TABLE t; $é$",
            "SELECT $$ set_config('a', 'b', false) $$",
            "SELECT $$ x $$ || $q$ FOR UPDATE $q$",
            "SELECT $_1$ ; DROP TABLE t; $_1$",
            "SELECT * FROM t WHERE a = $1 AND b = $2::int",
        ] {
            assert!(ro(s).is_ok(), "{s}");
        }
        for s in [
            "SELECT $a$ x $a$; DROP TABLE t; SELECT $a$ y $a$",
            "SELECT $$ x $$; DROP TABLE t; SELECT $$ y $$",
            // The mark must be exact: a different case does not close it.
            "SELECT $A$ x $a$ ; DROP TABLE t; $A$ ; DROP TABLE t",
            // A `$` glued to a word is part of the word, so nothing is quoted here.
            "SELECT a$b$c; DROP TABLE t; SELECT 1 AS a$b$",
            "SELECT 1 AS a$$; DROP TABLE t; --$$",
            // A `$` and a digit cannot open a quote, so the text between stays code.
            "SELECT $1$ ; DROP TABLE t; $1$",
        ] {
            assert!(ro(s).is_err(), "{s}");
        }
    }

    #[test]
    fn comments_hide_keywords_the_way_postgres_nests_them() {
        for s in [
            "SELECT 1 /* a /* b */ ; DROP TABLE t; */",
            "SELECT 1 /* ; DROP TABLE t */",
            "SELECT 1 --; DROP TABLE t",
            "SELECT 1 -- x\n",
            "SELECT 1 /*! DROP TABLE t */",
            "SELECT 1 /*+ DROP TABLE t */",
            "SELECT 1 /* a /* b /* c */ */ ; DROP TABLE t; */",
            "SELECT /* FOR UPDATE */ 1",
        ] {
            assert!(ro(s).is_ok(), "{s}");
        }
        for s in [
            "SELECT 1 /* a /* b */ */ ; DROP TABLE t",
            "SELECT 1 /* a */ ; DROP TABLE t; /* b */",
            "SELECT 1 /* a /* b */ c */ ; DROP TABLE t; /* d */",
            "SELECT 1 --x\n; DROP TABLE t",
            "SELECT 1 --x\r; DROP TABLE t",
            "SELECT 1 #x\n; DROP TABLE t",
            "SELECT 1 /**/ ; DROP TABLE t",
        ] {
            assert!(ro(s).is_err(), "{s}");
        }
    }

    #[test]
    fn escape_strings_and_plain_strings_are_read_under_both_settings() {
        for s in [
            r"SELECT E'it\'s'",
            r"SELECT E'\'; DROP TABLE t; --'",
            r"SELECT 'it''s'",
            r"SELECT E'a\\' || 'b'",
            r"SELECT U&'\0041', U&'a''b', U&'\+000061'",
            r#"SELECT U&"d\0061ta" FROM t"#,
            "SELECT B'101', X'1F', b'1', x'a'",
            // A backslash not before a quote reads the same either way.
            r"SELECT 'a\b'",
        ] {
            assert!(ro(s).is_ok(), "{s}: {:?}", ro(s));
        }
        for s in [
            // Under standard_conforming_strings on the first string ends at the quote after the backslash, off keeps it open.
            r"SELECT 'a\' , '; DROP TABLE t; --'",
            r"SELECT 'a\'; DROP TABLE t; --'",
            r"SELECT E'a\\'; DROP TABLE t; --'",
            r"SELECT U&'a\'; DROP TABLE t; --'",
            r"SELECT X'a\'; DROP TABLE t; --'",
            r"SELECT B'1'; DROP TABLE t",
            r"SELECT xE'a'; DROP TABLE t; SELECT 'b",
        ] {
            assert!(ro(s).is_err(), "{s}");
        }
    }

    #[test]
    fn ordinary_reads_pass() {
        for s in [
            "SELECT 1",
            "select * from t",
            "SELECT * FROM \"My Table\" WHERE \"insert\" = 1",
            "SELECT \"insert\", \"delete\", \"update\" FROM \"update\"",
            "SELECT a AS \"into\" FROM t",
            "SELECT update, delete, insert FROM t",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT * FROM r",
            "WITH x AS MATERIALIZED (SELECT 1) SELECT * FROM x ORDER BY 1 LIMIT ALL",
            "VALUES (1), (2)",
            "TABLE t",
            "SHOW search_path",
            "SHOW ALL",
            "EXPLAIN SELECT 1",
            "EXPLAIN VERBOSE SELECT 1",
            "EXPLAIN (VERBOSE, FORMAT JSON) SELECT 1",
            "EXPLAIN (COSTS off, BUFFERS) SELECT 1",
            "EXPLAIN WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 1; SELECT 2;",
            "(SELECT 1) UNION (SELECT 2)",
            "SELECT 1;;",
            "SELECT substring('abc' FROM 1 FOR 2)",
            "SELECT overlay('abc' PLACING 'x' FROM 2 FOR 1)",
            "SELECT 'for update', 'INTO', 'DROP TABLE t', 'set_config('",
            "SELECT * FROM t WHERE j ? 'a' AND j ?| array['a', 'b'] AND j ?& array['a'] AND j @> '{\"a\": 1}'::jsonb",
            "SELECT j #> '{a,b}', j #>> '{a}', j -> 'a' ->> 'b', j - 'a' FROM t",
            "SELECT 1::int, '2020-01-01'::date, a::text, (a)::numeric(10, 2), ARRAY[1, 2]::int[] FROM t",
            "SELECT * FROM t WHERE a = $1 AND b = $2::int OR c = ANY($3)",
            "SELECT count(*) FROM pg_stat_activity WHERE pid <> pg_backend_pid()",
            "SELECT currval('s'), lastval(), pg_sleep(0)",
            "SELECT 1 -- DROP TABLE t",
            "SELECT 1 /* DROP TABLE t */",
            "SELECT a || E'\\n' FROM t",
            "SELECT 'x' WHERE 1 < 2 AND 3 > 2 AND 1 <> 2 AND 1 != 2",
            "SELECT * FROM generate_series(1, 3) AS g(n)",
            "SELECT * FROM t t1 CROSS JOIN LATERAL (SELECT 1) l",
            "SELECT 'é' AS é, 日本 FROM t",
        ] {
            assert!(ro(s).is_ok(), "{s}: {:?}", ro(s));
        }
    }

    #[test]
    fn refusals_are_worded_like_the_mysql_ones() {
        assert_eq!(ro("DELETE FROM t").unwrap_err(), "DELETE writes data and this connection is read-only. Unlock writes first.");
        assert_eq!(ro("SET x = 1").unwrap_err(), "SET is not allowed on a read-only connection. Unlock writes first.");
        assert!(ro("WITH x AS (DELETE FROM t RETURNING 1) SELECT 1").unwrap_err().starts_with("DELETE inside WITH writes data and this connection is read-only."));
        assert!(ro("SELECT set_config('a', 'b', false)").unwrap_err().contains("set_config()"));
    }

    #[test]
    fn settings_that_change_how_text_is_read_are_caught() {
        for s in [
            "SET standard_conforming_strings = off",
            "set standard_conforming_strings to 'off'",
            "SET SESSION standard_conforming_strings = off",
            "SET LOCAL backslash_quote = on",
            "SET escape_string_warning = off",
            "SET client_encoding = 'SQL_ASCII'",
            "SET NAMES 'utf8'",
            "SET search_path TO other",
            "SET \"search_path\" = other",
            "SET SCHEMA 'other'",
            "SET default_transaction_read_only = off",
            "SET transaction_read_only = off",
            "SET SESSION AUTHORIZATION postgres",
            "SET ROLE postgres",
            "SET LOCAL role = postgres",
            "RESET ALL",
            "RESET role",
            "RESET standard_conforming_strings",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            "SET TRANSACTION READ WRITE",
            "SELECT set_config('standard_conforming_strings', 'off', false)",
            "SELECT pg_catalog.set_config('search_path', 'x', false)",
            "SELECT \"set_config\" /* c */ ('role', 'x', false)",
            "DO $$ BEGIN SET standard_conforming_strings = off; END $$",
            "DISCARD ALL",
            "SELECT 1; SET ROLE x",
            "BEGIN; SET LOCAL client_encoding = 'x'",
            "SELECT 'a\\' ; SET ROLE x",
        ] {
            assert!(changes_escaping(s, PG), "{s}");
        }
        for s in [
            "SELECT 1",
            "SELECT 'SET ROLE x'",
            "/* SET ROLE x */ SELECT 1",
            "SELECT $$ SET ROLE x $$",
            "SET statement_timeout = 1000",
            "SET TIME ZONE 'UTC'",
            "SET LOCAL lock_timeout = '1s'",
            "SET work_mem = '64MB'",
            "SET application_name = 'x'",
            "SELECT * FROM t WHERE role = 'x'",
        ] {
            assert!(!changes_escaping(s, PG), "{s}");
        }
    }

    #[test]
    fn a_refused_name_is_refused_after_a_dot_too() {
        for s in [
            "SELECT (42::bigint).pg_advisory_lock",
            "SELECT x.pg_advisory_lock FROM generate_series(11::bigint,12) x",
            "SELECT ('dbname=postgres'::text).dblink_connect",
            "SELECT (pg_backend_pid()).pg_cancel_backend",
            "SELECT (true).pg_promote",
            "SELECT ('bgwriter'::text).pg_stat_reset_shared",
            "SELECT ('/etc/passwd'::text).pg_read_file",
            "SELECT (1).\"pg_advisory_lock\"",
            "SELECT (1).PG_ADVISORY_LOCK",
            "SELECT (1).\"PG_ADVISORY_LOCK\"",
            "SELECT (1). /* c */ pg_advisory_lock",
            "SELECT (1).-- c\npg_advisory_lock",
            "SELECT (1) . pg_advisory_lock",
            "SELECT t.nextval FROM t",
            "SELECT t.lo_unlink FROM t",
            "SELECT t.pg_replication_slot_advance FROM t",
            "SELECT t.pg_ls_logdir FROM t",
            "SELECT (1).run_command_on_workers",
            "SELECT 'a\\' ; SELECT (1).pg_advisory_lock",
        ] {
            assert!(ro(s).is_err(), "{s}");
        }
        // Columns and catalog relations whose names start like a refused family stay readable.
        for s in [
            "SELECT t.lo_price FROM t",
            "SELECT lo_price FROM t",
            "SELECT t.pg_file_name FROM t",
            "SELECT * FROM pg_catalog.pg_file_settings",
            "SELECT * FROM pg_catalog.pg_replication_slots",
            "SELECT * FROM pg_catalog.pg_replication_origin_status",
            "SELECT * FROM pg_catalog.pg_stat_replication_slots",
            "SELECT t.setval_x, t.crosstab_id, t.lowrite_at FROM t",
        ] {
            assert!(ro(s).is_ok(), "{s}: {:?}", ro(s));
        }
    }

    #[test]
    fn functions_that_run_sql_text_are_refused() {
        for s in [
            "SELECT * FROM ts_stat($$SELECT to_tsvector(pg_advisory_lock(7)::text)$$)",
            "SELECT ts_rewrite('a'::tsquery, 'SELECT ''a''::tsquery, ''b''::tsquery FROM (SELECT pg_advisory_lock(8)) s')",
            "SELECT * FROM crosstab($$SELECT 1::text, 2::text, 3::text$$) AS c(a text, b text)",
            "SELECT * FROM connectby('t', 'k', 'p', 'a', 0) AS c(k text, p text, l int)",
            "SELECT pg_background_launch('SELECT 1')",
            "SELECT query_to_xml_and_xmlschema('SELECT 1', true, true, '')",
        ] {
            assert!(ro(s).is_err(), "{s}");
        }
    }

    #[test]
    fn explain_with_analyze_switched_off_is_a_plan_only() {
        for s in ["EXPLAIN (ANALYZE false) SELECT 1", "EXPLAIN (ANALYZE off) SELECT 1", "EXPLAIN (ANALYZE 0) SELECT 1", "EXPLAIN (ANALYZE FALSE, VERBOSE) SELECT 1", "EXPLAIN (VERBOSE, analyse off, FORMAT JSON) SELECT 1", "EXPLAIN (ANALYZE false) SELECT * FROM t"] {
            assert!(ro(s).is_ok(), "{s}: {:?}", ro(s));
        }
        // The inner statement is still checked.
        for s in ["EXPLAIN (ANALYZE false) DELETE FROM t", "EXPLAIN (ANALYZE false) SELECT pg_advisory_lock(1)"] {
            assert!(ro(s).is_err(), "{s}");
        }
    }

    #[test]
    fn updating_pg_settings_changes_escaping() {
        for s in ["UPDATE pg_settings SET setting = 'off' WHERE name = 'standard_conforming_strings'", "update pg_catalog.pg_settings set setting = 'x'", "UPDATE \"pg_settings\" SET setting = 'x'", "SELECT 1; UPDATE pg_settings SET setting = 'x'", "WITH x AS (UPDATE pg_settings SET setting = 'x' RETURNING 1) SELECT * FROM x",
            // A Unicode-escaped table name, a view made over pg_settings, and a setting changed inside SQL text.
            "UPDATE U&\"pg_setting\\0073\" SET setting='off' WHERE name='standard_conforming_strings'", "CREATE VIEW v AS SELECT * FROM pg_settings",
            "SELECT * FROM ts_stat($$SELECT to_tsvector(set_config('client_encoding','SJIS',false))$$)", "SELECT * FROM crosstab('x') AS c(a int)"] {
            assert!(changes_escaping(s, PG), "{s}");
        }
        for s in ["SELECT * FROM pg_settings", "UPDATE t SET a = 1", "SELECT 'UPDATE pg_settings'"] {
            assert!(!changes_escaping(s, PG), "{s}");
        }
    }

    #[test]
    fn prepared_statements_are_found_under_both_readings() {
        for s in ["PREPARE p AS SELECT 1", "execute p", "DEALLOCATE ALL", "SELECT 1; EXECUTE p", "/* c */ PREPARE p AS SELECT 1", "SELECT 'a\\' ; PREPARE p AS SELECT 1"] {
            assert!(uses_prepared(s, PG), "{s}");
        }
        for s in ["SELECT 1", "SELECT 'PREPARE p'", "SELECT $$ EXECUTE p $$", "SELECT prepare FROM t"] {
            assert!(!uses_prepared(s, PG), "{s}");
        }
    }

    #[test]
    fn the_lists_are_lowercase_and_cover_what_the_tests_spell() {
        for n in REFUSED_FUNCTIONS_EXACT.iter().chain(REFUSED_FUNCTIONS_PREFIX).chain(REFUSED_FUNCTIONS_CALL_PREFIX).chain(REFUSED_FUNCTIONS_CONTAIN) {
            assert_eq!(*n, n.to_ascii_lowercase());
        }
        for f in FUNCTIONS {
            assert!(refused_function(f), "{f}");
        }
    }

    #[test]
    fn a_data_modifying_cte_is_not_pageable_at_any_depth() {
        use super::super::{pageable, transaction_control};
        for s in [
            "WITH d AS (DELETE FROM t WHERE a = 1 RETURNING *) SELECT * FROM d",
            "with i as (insert into t values (1) returning *) select * from i",
            "WITH a AS (SELECT 1), b AS (UPDATE t SET a = 2 RETURNING a) SELECT * FROM b",
            "WITH x AS (WITH y AS (DELETE FROM t RETURNING *) SELECT * FROM y) SELECT * FROM x",
            "WITH m AS (MERGE INTO t USING s ON true WHEN MATCHED THEN DO NOTHING RETURNING *) SELECT * FROM m",
        ] {
            assert!(!pageable(s, PG), "{s}");
        }
        for s in ["SELECT * FROM t", "WITH a AS (SELECT 1 AS x) SELECT * FROM a", "SELECT \"delete\" FROM t", "SELECT 'insert into' AS s"] {
            assert!(pageable(s, PG), "{s}");
        }
        for s in ["BEGIN", "begin read only", "START TRANSACTION", "COMMIT", "END", "ROLLBACK", "ABORT", "SAVEPOINT a", "RELEASE a", "PREPARE TRANSACTION 'x'", "/* c */ commit", "COMMIT PREPARED 'x'"] {
            assert!(transaction_control(s, PG), "{s}");
        }
        for s in ["SELECT 1", "PREPARE p AS SELECT 1", "INSERT INTO t VALUES (1)", "SELECT 'BEGIN'", "WITH a AS (SELECT 1) SELECT * FROM a", "EXPLAIN SELECT 1"] {
            assert!(!transaction_control(s, PG), "{s}");
        }
        assert!(!transaction_control("BEGIN", Dialect::MySql));
    }

    #[test]
    fn statements_that_leave_session_state_are_seen() {
        use super::super::session_state;
        for s in [
            "SET search_path = x", "set statement_timeout = 0", "SET SESSION ROLE r", "SET ROLE r", "RESET ALL", "DISCARD ALL", "LISTEN c", "UNLISTEN *",
            "PREPARE p AS SELECT 1", "DEALLOCATE p", "LOAD 'auto_explain'", "DECLARE c CURSOR WITH HOLD FOR SELECT 1", "CREATE TEMP TABLE t (a int)",
            "create temporary table t (a int)", "CREATE LOCAL TEMP TABLE t (a int)", "SELECT 1 INTO TEMP t", "SELECT set_config('x.y', '1', false)",
            "SELECT pg_catalog.\"set_config\"('x.y', '1', true)", "/* c */ SET x.y = 1",
        ] {
            assert!(session_state(s, PG), "{s}");
        }
        for s in ["SET LOCAL search_path = x", "SET TRANSACTION READ ONLY", "SET CONSTRAINTS ALL DEFERRED", "DECLARE c CURSOR FOR SELECT 1", "CREATE TABLE t (a int)", "INSERT INTO t VALUES (1)", "SELECT 'SET x = 1'", "UPDATE t SET a = 1"] {
            assert!(!session_state(s, PG), "{s}");
        }
        assert!(!session_state("SET x = 1", Dialect::MySql));
    }

    #[test]
    fn a_do_block_and_anything_in_pg_temp_leave_state_wherever_they_sit_in_the_statement() {
        use super::super::session_state;
        for s in [
            "DO $$BEGIN PERFORM pg_advisory_lock(424243); END$$", "do language plpgsql $$ begin create temp table x(a int); end $$", "DO $$ BEGIN CREATE TABLE pg_temp.x (a int); END $$",
            "CREATE TABLE pg_temp.x (a int)", "CREATE TABLE \"pg_temp\".x (a int)", "CREATE TABLE pg_temp_3.x (a int)", "CREATE TABLE x AS SELECT 1 FROM pg_temp.y", "SELECT * INTO pg_temp.x FROM t",
            "CREATE FUNCTION pg_temp.f() RETURNS int LANGUAGE sql AS 'select 1'", "WITH a AS (SELECT 1) SELECT * INTO pg_temp.z FROM a", "CREATE /* c */ VIEW pg_temp.v AS SELECT 1",
            // A DO block that only looks like the one Quarry writes.
            "DO $q$BEGIN CREATE TYPE a.b AS ENUM ('x'); PERFORM pg_advisory_lock(1); EXCEPTION WHEN duplicate_object THEN NULL; END$q$",
            "DO $q$BEGIN CREATE TABLE pg_temp.t (a int); EXCEPTION WHEN duplicate_object THEN NULL; END$q$", "DO $q$BEGIN CREATE TYPE a.b AS ENUM ('x'); EXCEPTION WHEN others THEN NULL; END$q$",
            "DO $q$BEGIN SELECT set_config('a.b', '1', false); EXCEPTION WHEN duplicate_object THEN NULL; END$q$",
        ] {
            assert!(session_state(s, PG), "{s}");
        }
        // The blocks a dump of some tables holds, and a name that only contains the word.
        for s in [
            "DO $quarry$BEGIN CREATE TYPE \"app\".\"mood\" AS ENUM ('a', 'b'); EXCEPTION WHEN duplicate_object THEN NULL; END$quarry$",
            "DO $quarry$BEGIN CREATE DOMAIN \"app\".\"d\" AS integer DEFAULT 1 NOT NULL; EXCEPTION WHEN duplicate_object THEN NULL; END$quarry$",
            "DO $quarry$BEGIN ALTER DOMAIN \"app\".\"d\" ADD CONSTRAINT \"c\" CHECK (VALUE > 0); EXCEPTION WHEN duplicate_object THEN NULL; END$quarry$",
            "CREATE TABLE my_pg_temp_x (a int)", "SELECT 'pg_temp.x'", "SELECT 1 FROM t WHERE a = $$pg_temp$$",
        ] {
            assert!(!session_state(s, PG), "{s}");
        }
    }

    #[test]
    fn session_advisory_locks_and_dblink_are_session_state_in_every_spelling() {
        use super::super::session_state;
        for s in [
            "SELECT pg_advisory_lock(42)", "select PG_ADVISORY_LOCK(1, 2)", "SELECT pg_advisory_lock_shared(42)", "SELECT pg_try_advisory_lock(42)", "SELECT pg_try_advisory_lock_shared(1, 2)",
            "SELECT pg_catalog.pg_advisory_lock(42)", "SELECT \"pg_catalog\".\"pg_advisory_lock\"(42)", "SELECT pg_catalog . pg_advisory_lock (42)", "SELECT (42).pg_advisory_lock", "SELECT x.pg_try_advisory_lock FROM (SELECT 1) x",
            "SELECT /* c */ pg_advisory_lock(42)", "SELECT 1; SELECT pg_advisory_lock(42)", "WITH a AS (SELECT pg_advisory_lock(42)) SELECT * FROM a",
            "SELECT dblink_connect('c', 'host=x')", "SELECT dblink('host=x', 'select 1')", "SELECT public.dblink_exec('c', 'x')", "SELECT \"dblink_open\"('c', 'k', 'select 1')", "SELECT * FROM dblink('x', 'y') AS t(a int)",
        ] {
            assert!(session_state(s, PG), "{s}");
        }
        // The transaction-scoped locks end with the transaction, and a string or a name that only looks the same is not a call.
        for s in [
            "SELECT pg_advisory_xact_lock(42)", "SELECT pg_try_advisory_xact_lock(42)", "SELECT pg_advisory_xact_lock_shared(42)", "SELECT 'pg_advisory_lock(42)'", "SELECT pg_advisory_lock FROM t",
            "SELECT dblink_x FROM t", "SELECT $$dblink('x')$$", "SELECT 1 -- pg_advisory_lock(1)", "SELECT pg_advisory_unlock(42)",
        ] {
            assert!(!session_state(s, PG), "{s}");
        }
    }

    #[test]
    fn statements_the_server_refuses_inside_a_transaction_block_are_seen() {
        use super::super::needs_no_transaction;
        for s in [
            "VACUUM", "vacuum full t", "VACUUM (ANALYZE) t", "/* c */ VACUUM t",
            "CREATE INDEX CONCURRENTLY i ON t (a)", "create unique index concurrently i on t (a)", "DROP INDEX CONCURRENTLY i", "DROP INDEX CONCURRENTLY IF EXISTS i",
            "REINDEX INDEX CONCURRENTLY i", "REINDEX TABLE CONCURRENTLY t", "REINDEX (CONCURRENTLY) TABLE t", "REINDEX (VERBOSE, CONCURRENTLY) SCHEMA s", "REINDEX SCHEMA s", "REINDEX DATABASE d", "REINDEX SYSTEM d",
            "CREATE DATABASE d", "DROP DATABASE d", "drop database if exists d", "ALTER SYSTEM SET work_mem = '5MB'", "ALTER SYSTEM RESET ALL", "CREATE TABLESPACE s LOCATION '/x'", "DROP TABLESPACE s",
            "ALTER DATABASE d SET TABLESPACE s", "ALTER TABLE t DETACH PARTITION p CONCURRENTLY", "CREATE SUBSCRIPTION s CONNECTION 'x' PUBLICATION p", "DROP SUBSCRIPTION s",
            "CLUSTER", "cluster verbose", "CLUSTER (VERBOSE)", "CLUSTER;",
        ] {
            assert!(needs_no_transaction(s, PG), "{s}");
        }
        // These run in a block on every supported server, so they stay allowed. ALTER TYPE ADD VALUE and the CONCURRENTLY refresh were checked on 13 and 18.
        for s in [
            "ALTER TYPE mood ADD VALUE 'x'", "REFRESH MATERIALIZED VIEW CONCURRENTLY mv", "REFRESH MATERIALIZED VIEW mv", "CLUSTER t USING i", "CLUSTER t", "CLUSTER VERBOSE t", "REINDEX TABLE t", "REINDEX INDEX i",
            "CREATE INDEX i ON t (a)", "DROP INDEX i", "CREATE TABLE concurrently_t (a int)", "ALTER TABLE t DETACH PARTITION p", "ALTER TABLE t ADD COLUMN a int", "ANALYZE", "CHECKPOINT", "SELECT 'VACUUM'", "SELECT 1; VACUUM",
            "INSERT INTO t VALUES (1)", "CREATE SCHEMA s", "DROP TABLE t", "ALTER DATABASE d SET work_mem = '5MB'", "ALTER ROLE r SET x = 1", "SELECT * FROM vacuum",
        ] {
            assert!(!needs_no_transaction(s, PG), "{s}");
        }
        assert!(!needs_no_transaction("VACUUM", Dialect::MySql));
    }
}
