//! SQL autocomplete: what to suggest for the text before the caret, and the input that shows it.
//! The lists come from the cached schema of the database, the lexer, and values looked up on the
//! server. The popup itself is the one the Mongo inputs use.

use super::complete::{ac_input_with, fuzzy, Ac, Fields, SqlInfo, Sug};
use super::sql_words_pg::{FUNCTIONS_PG, KEYWORDS_PG, RESERVED_PG, TYPES_PG};
use super::theme::*;
use crate::db::sql::{
    lex::{self, Tok},
    schema::{enum_values, Schema, SchemaCol, SchemaTable},
    value::Kind,
    Dialect, FUNCTIONS, KEYWORDS,
};
use eframe::egui::{self, text::LayoutJob};
use std::cell::RefCell;
use std::sync::Arc;

const MAX_SUGGESTIONS: usize = 50;

/// Words MySQL will not take as a bare name, so a table or column called this gets backticks.
const RESERVED: &[&str] = &[
    "ADD", "ALL", "ALTER", "ANALYZE", "AND", "AS", "ASC", "BETWEEN", "BY", "CASE", "CHANGE", "CHECK", "COLLATE", "COLUMN", "CONSTRAINT", "CREATE", "CROSS", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP",
    "DATABASE", "DEFAULT", "DELETE", "DESC", "DESCRIBE", "DISTINCT", "DROP", "ELSE", "EXISTS", "EXPLAIN", "FALSE", "FOR", "FOREIGN", "FROM", "GRANT", "GROUP", "HAVING", "IF", "IGNORE", "IN", "INDEX", "INNER", "INSERT",
    "INTERVAL", "INTO", "IS", "JOIN", "KEY", "KEYS", "KILL", "LEFT", "LIKE", "LIMIT", "LOCK", "MATCH", "NOT", "NULL", "ON", "OPTION", "OR", "ORDER", "OUTER", "PRIMARY", "RANGE", "RANK", "READ", "REFERENCES", "REGEXP",
    "RENAME", "REPLACE", "RIGHT", "ROW", "ROWS", "SCHEMA", "SELECT", "SET", "SHOW", "SYSTEM", "TABLE", "THEN", "TO", "TRIGGER", "TRUE", "UNION", "UNIQUE", "UPDATE", "USE", "USING", "VALUES", "WHEN", "WHERE", "WITH",
];

fn keywords(dl: Dialect) -> &'static [&'static str] {
    if dl == Dialect::Postgres { KEYWORDS_PG } else { KEYWORDS }
}

fn functions(dl: Dialect) -> &'static [(&'static str, &'static str)] {
    if dl == Dialect::Postgres { FUNCTIONS_PG } else { FUNCTIONS }
}

/// What completion knows about the place being edited. `table` is the implicit table of the Content
/// bar, whose text has no FROM; `fallback` holds its columns until the schema has arrived.
#[derive(Default)]
pub(crate) struct Scope<'a> {
    pub(crate) schema: Arc<Schema>,
    pub(crate) db: String,
    /// The databases of the server with their table lists, once loaded.
    pub(crate) dbs: &'a [(String, Option<Vec<String>>)],
    pub(crate) table: Option<String>,
    pub(crate) fallback: Arc<Vec<SchemaCol>>,
    /// Names of the views of `db` that the sidebar knows, so the plain table list shown before the schema arrives still marks them.
    pub(crate) views: Arc<std::collections::HashSet<String>>,
    /// How identifiers are quoted when a suggestion is inserted.
    pub(crate) dialect: Dialect,
    /// The schemas of a PostgreSQL database, so `sch` and `sch.` can be completed. Empty until the sidebar loads them.
    pub(crate) schemas: Vec<String>,
}

/// A lookup the caret asks the app to make: the schema of the database, or a column's values.
#[derive(Default, Clone, PartialEq, Debug)]
pub(crate) struct Wants {
    pub(crate) schema: bool,
    pub(crate) values: Option<Need>,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Need {
    pub(crate) table: String,
    pub(crate) column: String,
    /// The text typed so far, when the list of frequent values did not hold it.
    pub(crate) prefix: Option<String>,
}

/// Key of a column's values in the `Fields` the app keeps.
pub(crate) fn value_key(table: &str, column: &str) -> String {
    format!("{table}.{column}")
}

/// The suggestions for a place in a statement, with the heading and footer the popup shows and the
/// lookup that would make them better.
#[derive(Default)]
pub(crate) struct Out {
    pub(crate) from: usize,
    pub(crate) sugs: Vec<Sug>,
    pub(crate) title: String,
    pub(crate) foot: String,
    pub(crate) need: Option<Need>,
}

/// A significant token: not space and not a comment.
#[derive(Clone, Copy)]
struct Tk<'a> {
    tok: Tok,
    s: &'a str,
    start: usize,
}

fn sig<'a>(text: &'a str, toks: &[(Tok, std::ops::Range<usize>)]) -> Vec<Tk<'a>> {
    toks.iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).map(|(t, r)| Tk { tok: *t, s: &text[r.clone()], start: r.start }).collect()
}

impl Tk<'_> {
    fn is(&self, kw: &str) -> bool {
        self.tok == Tok::Word && self.s.eq_ignore_ascii_case(kw)
    }

    fn punct(&self, p: &str) -> bool {
        self.tok == Tok::Punct && self.s == p
    }

    /// The name this token spells, backticks removed.
    fn name(&self) -> Option<String> {
        match self.tok {
            Tok::Word if !self.s.starts_with(|c: char| c.is_ascii_digit() || c == '@' || c == '$') => Some(self.s.to_string()),
            Tok::Quoted => {
                let q = self.s.chars().next().filter(|c| matches!(c, '`' | '"'))?;
                let inner = self.s.strip_prefix(q)?.strip_suffix(q)?;
                Some(inner.replace(&format!("{q}{q}"), &q.to_string()))
            }
            _ => None,
        }
    }
}

fn needs_quote(dl: Dialect, name: &str) -> bool {
    // PostgreSQL folds a bare name to lower case, so anything else has to be quoted to keep its spelling.
    if dl == Dialect::Postgres {
        return name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') || RESERVED_PG.contains(&name.to_ascii_uppercase().as_str());
    }
    name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') || RESERVED.contains(&name.to_ascii_uppercase().as_str())
}

/// A name as it is inserted. A backtick already typed is closed instead of added.
fn ident(dl: Dialect, name: &str, in_tick: bool) -> String {
    if in_tick {
        dl.quote_ident(name)[1..].to_string()
    } else if needs_quote(dl, name) {
        dl.quote_ident(name)
    } else {
        name.to_string()
    }
}

fn starts_ci(name: &str, word: &str) -> bool {
    name.len() >= word.len() && name.as_bytes()[..word.len()].eq_ignore_ascii_case(word.as_bytes())
}

/// Columns that start with `word`, then, for two letters or more, those that hold its letters in order
/// after the same first letter, like the popup's own highlighting reads them.
fn col_matches<'a>(cols: &'a [SchemaCol], word: &str) -> Vec<&'a SchemaCol> {
    let mut out: Vec<&SchemaCol> = cols.iter().filter(|c| starts_ci(&c.name, word)).collect();
    if word.chars().count() >= 2 {
        out.extend(cols.iter().filter(|c| !starts_ci(&c.name, word) && fuzzy(word, &c.name, true)));
    }
    out
}

/// A keyword in the case the user is typing: upper unless the word so far is all lower case.
fn kw_case(word: &str, kw: &str) -> String {
    if word.is_empty() || word.chars().any(|c| c.is_ascii_uppercase()) { kw.to_string() } else { kw.to_ascii_lowercase() }
}

/// "INT UNSIGNED", "VARCHAR(255)"; the member list of an enum is left out.
fn type_label(c: &SchemaCol) -> String {
    match c.kind {
        Kind::Enum => "ENUM".into(),
        Kind::Set => "SET".into(),
        _ => c.ty.to_ascii_uppercase(),
    }
}

fn sug(text: String, note: &'static str, insert: String, detail: String, foot: String) -> Sug {
    let caret = insert.chars().count();
    Sug { sql: Some(SqlInfo { detail, foot }), back: 0, text, note, insert, caret }
}

/// A table named in FROM, JOIN, UPDATE or INTO, with its alias.
struct Ref {
    db: Option<String>,
    table: String,
    alias: Option<String>,
}

/// The tables a statement reads from or writes to, in order, found anywhere in it so a SELECT list
/// before the FROM still sees its aliases.
fn parse_refs(toks: &[Tk], dl: Dialect) -> Vec<Ref> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let from = toks[i].is("FROM");
        if !(from || toks[i].is("JOIN") || toks[i].is("UPDATE") || toks[i].is("INTO")) {
            i += 1;
            continue;
        }
        i += 1;
        while let Some(first) = toks.get(i).and_then(Tk::name) {
            i += 1;
            let (db, table) = match (toks.get(i), toks.get(i + 1).and_then(Tk::name)) {
                (Some(d), Some(t)) if d.punct(".") => {
                    i += 2;
                    (Some(first), t)
                }
                _ => (None, first),
            };
            let mut alias = None;
            if toks.get(i).is_some_and(|t| t.is("AS")) {
                alias = toks.get(i + 1).and_then(Tk::name);
                i += 2;
            } else if let Some(t) = toks.get(i).filter(|t| t.tok == Tok::Quoted || (t.tok == Tok::Word && !keywords(dl).contains(&t.s.to_ascii_uppercase().as_str()))) {
                alias = t.name();
                i += 1;
            }
            out.push(Ref { db, table, alias });
            if from && toks.get(i).is_some_and(|t| t.punct(",")) {
                i += 1;
                continue;
            }
            break;
        }
    }
    out
}

/// A table in scope: the name it is called by (its alias, else its own), its own name and columns.
struct Rt<'a> {
    label: String,
    table: String,
    cols: &'a [SchemaCol],
}

fn table_cols<'a>(scope: &'a Scope, name: &str) -> Option<(&'a str, &'a [SchemaCol])> {
    if let Some(t) = scope.schema.table(name) {
        return Some((&t.name, &t.cols));
    }
    let own = scope.table.as_deref().filter(|t| t.eq_ignore_ascii_case(name) && !scope.fallback.is_empty())?;
    Some((own, scope.fallback.as_slice()))
}

fn resolve<'a>(refs: &[Ref], scope: &'a Scope) -> Vec<Rt<'a>> {
    let mut out: Vec<Rt> = refs
        .iter()
        .filter(|r| r.db.as_deref().is_none_or(|d| d.eq_ignore_ascii_case(&scope.db)))
        .filter_map(|r| {
            let (name, cols) = table_cols(scope, &r.table)?;
            Some(Rt { label: r.alias.clone().unwrap_or_else(|| r.table.clone()), table: name.to_string(), cols })
        })
        .collect();
    if refs.is_empty() {
        out.extend(scope.table.as_deref().and_then(|t| table_cols(scope, t)).map(|(n, c)| Rt { label: n.to_string(), table: n.to_string(), cols: c }));
    }
    out
}

fn by_qualifier<'a, 'b>(rts: &'b [Rt<'a>], q: &str) -> Option<&'b Rt<'a>> {
    rts.iter().find(|r| r.label.eq_ignore_ascii_case(q)).or_else(|| rts.iter().find(|r| r.table.eq_ignore_ascii_case(q)))
}

/// Reads the qualified name that ends just before token `end`: `o.status` gives ["o", "status"].
fn operand(toks: &[Tk], end: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut k = end;
    while k > 0 && parts.len() < 3 {
        let Some(n) = toks[k - 1].name() else { break };
        parts.insert(0, n);
        if k >= 3 && toks[k - 2].punct(".") {
            k -= 2;
        } else {
            break;
        }
    }
    parts
}

/// The clause keyword the caret is in, skipping balanced parentheses.
fn clause(toks: &[Tk]) -> Option<String> {
    const CLAUSES: &[&str] = &["SELECT", "FROM", "WHERE", "JOIN", "ON", "GROUP", "ORDER", "HAVING", "SET", "UPDATE", "INTO", "VALUES", "LIMIT", "USING"];
    let mut depth = 0;
    for t in toks.iter().rev() {
        if t.punct(")") {
            depth += 1;
        } else if t.punct("(") {
            depth = (depth - 1).max(0);
        } else if depth == 0 && t.tok == Tok::Word && let Some(c) = CLAUSES.iter().find(|c| t.s.eq_ignore_ascii_case(c)) {
            return Some(c.to_string());
        }
    }
    None
}

/// The percentages of a column's values, when the lookup counted them.
fn percent(info: &super::complete::FieldInfo, i: usize) -> String {
    let total: u64 = info.freq.iter().sum();
    match info.freq.get(i) {
        Some(&n) if total > 0 => match n * 100 / total {
            0 => "<1%".into(),
            p => format!("{p}%"),
        },
        _ => String::new(),
    }
}

/// "Values: 'EUR' 61% 'USD' 27% 'GBP' 12%" for the footer, from the looked up values.
fn values_foot(values: &Fields, table: &str, col: &str) -> String {
    let Some(info) = values.get(&value_key(table, col)) else { return String::new() };
    let parts: Vec<String> = info.values.iter().take(3).enumerate().filter_map(|(i, v)| Some(format!("'{}' {}", v.as_str()?, percent(info, i)).trim_end().to_string())).collect();
    if parts.is_empty() { String::new() } else { format!("Values: {}", parts.join("  ")) }
}

fn col_sug(dl: Dialect, c: &SchemaCol, table: &str, scope_values: &Fields, qualifier: Option<&str>, in_tick: bool) -> Sug {
    let mut detail = type_label(c);
    if c.key == "UNI" {
        detail += " · UNIQUE";
    }
    if c.fk {
        detail += " · FK";
    }
    let name = ident(dl, &c.name, in_tick);
    let insert = match qualifier {
        Some(q) => format!("{}.{name}", ident(dl, q, false)),
        None => name,
    };
    let text = qualifier.map_or_else(|| c.name.clone(), |q| format!("{q}.{}", c.name));
    sug(text, if c.key == "PRI" { "pk" } else { "column" }, insert, detail, values_foot(scope_values, table, &c.name))
}

fn table_sug(dl: Dialect, t: &SchemaTable, in_tick: bool) -> Sug {
    let detail = if t.view { "view".to_string() } else { t.rows.map_or(String::new(), |n| format!("{} rows", crate::app::group(&n.to_string()))) };
    sug(t.name.clone(), if t.view { "view" } else { "table" }, ident(dl, &t.name, in_tick), detail, String::new())
}

/// The text of the statement the caret is in, up to the caret and whole. `before` and `tail` are the
/// script before and after the caret. They are lexed together, so a `;` inside a string does not split.
pub(crate) fn split_statement(before: &str, tail: &str, dl: Dialect) -> (String, String) {
    let all = format!("{before}{tail}");
    let semis: Vec<usize> = lex::tokens(&all, dl).into_iter().filter(|(t, r)| *t == Tok::Punct && &all[r.clone()] == ";").map(|(_, r)| r.start).collect();
    let start = semis.iter().rev().find(|&&p| p < before.len()).map_or(0, |p| p + 1);
    let end = semis.iter().find(|&&p| p >= before.len()).copied().unwrap_or(all.len());
    (all[start..before.len()].to_string(), all[start..end].to_string())
}

/// Suggestions for the text before the caret, with the heading, footer and lookups. None where nothing should pop up: in a comment, in a string
/// that is not a value, in a closed identifier, or in an empty word where a list would only be noise.
pub(crate) fn analyze(before: &str, full_stmt: &str, scope: &Scope, values: &Fields) -> Option<Out> {
    // A space after the text shows whether it ends inside a string, comment or backtick name: such a
    // token swallows the space, while a finished one leaves it as a token of its own.
    let probe = format!("{before} ");
    let ptoks = lex::tokens(&probe, scope.dialect);
    let (open, orng) = ptoks.last().filter(|(t, r)| r.start < before.len() && r.end == probe.len() && matches!(t, Tok::Str | Tok::Quoted | Tok::Comment)).map(|(t, r)| (Some(*t), r.clone())).unwrap_or((None, 0..0));
    let toks = lex::tokens(before, scope.dialect);
    let prev_all = sig(before, &toks);
    // A backtick typed and not yet closed would swallow the rest of the statement, aliases included.
    let closed;
    let full_stmt = if open == Some(Tok::Quoted) && full_stmt.starts_with(before) {
        closed = format!("{before}{}{}", if scope.dialect == Dialect::Postgres { '"' } else { '`' }, &full_stmt[before.len()..]);
        closed.as_str()
    } else {
        full_stmt
    };
    let full_toks = lex::tokens(full_stmt, scope.dialect);
    let full_sig = sig(full_stmt, &full_toks);
    let rts = resolve(&parse_refs(&full_sig, scope.dialect), scope);
    match open {
        Some(Tok::Comment) => return None,
        Some(Tok::Str) => {
            // A dollar quoted body is text, and a prefix such as E or B sits before the opening quote.
            let qoff = before[orng.start..].find(['\'', '"']).filter(|_| !before[orng.start..].starts_with('$'))?;
            let upto = prev_all.iter().position(|t| t.start >= orng.start).unwrap_or(prev_all.len());
            let at = orng.start + qoff;
            return value_list(&prev_all[..upto], &before[at + 1..], at + 1, &before[at..at + 1], &rts, values);
        }
        _ => {}
    }
    let in_tick = open == Some(Tok::Quoted);
    // The word being typed, and the tokens before it.
    let (word, from, prev) = if in_tick {
        let upto = prev_all.iter().position(|t| t.start >= orng.start).unwrap_or(prev_all.len());
        let at = orng.start + before[orng.start..].find(['`', '"']).unwrap_or(0);
        (&before[at + 1..], at + 1, &prev_all[..upto])
    } else {
        match prev_all.last() {
            Some(t) if t.tok == Tok::Word && t.start + t.s.len() == before.len() && !t.s.starts_with(['@', '$']) => (t.s, t.start, &prev_all[..prev_all.len() - 1]),
            _ => ("", before.len(), &prev_all[..]),
        }
    };
    // A number typed after a comparison: the values of a numeric column, which go in unquoted.
    if !in_tick && word.starts_with(|c: char| c.is_ascii_digit()) && let Some(out) = value_list(prev, word, from, "", &rts, values) {
        return Some(out);
    }
    // After `::` PostgreSQL wants a type, and nothing else fits.
    if !in_tick && scope.dialect == Dialect::Postgres && prev.len() >= 2 && prev[prev.len() - 1].punct(":") && prev[prev.len() - 2].punct(":") && prev[prev.len() - 2].start + 1 == prev[prev.len() - 1].start {
        let sugs: Vec<Sug> = TYPES_PG.iter().filter(|t| starts_ci(t, word)).map(|t| sug((*t).to_string(), "type", (*t).to_string(), String::new(), String::new())).collect();
        return (!sugs.is_empty() && !(sugs.len() == 1 && sugs[0].text.eq_ignore_ascii_case(word))).then_some(Out { from, sugs, ..Default::default() });
    }
    if !in_tick && word.is_empty() && !prev.last().is_some_and(|t| t.punct(".")) {
        return None;
    }
    // After `x.`: the columns of table or alias x, or the tables of database x.
    if prev.last().is_some_and(|t| t.punct(".")) {
        let parts = operand(prev, prev.len() - 1);
        return match parts.as_slice() {
            [q] => qualified(q, word, from, in_tick, scope, &rts, values),
            [d, t] if d.eq_ignore_ascii_case(&scope.db) => by_qualifier_cols(t, word, from, in_tick, scope, values),
            _ => None,
        };
    }
    let last = prev.last();
    // After a table keyword, or a comma in a FROM list: tables.
    if last.is_some_and(|t| ["FROM", "JOIN", "UPDATE", "INTO", "TABLE"].iter().any(|k| t.is(k))) || last.is_some_and(|t| t.punct(",")) && clause(prev).as_deref() == Some("FROM") {
        return Some(tables_list(word, from, in_tick, scope));
    }
    // Right after a table name: an alias or the next keyword, so only keywords fit.
    let after_table = last.is_some_and(|t| t.name().is_some()) && {
        let mut k = prev.len() - 1;
        if k >= 2 && prev[k - 1].punct(".") {
            k -= 2;
        }
        k > 0 && ["FROM", "JOIN", "UPDATE", "INTO"].iter().any(|kw| prev[k - 1].is(kw))
    };
    if last.is_some_and(|t| t.is("AS")) {
        return None;
    }
    let mut sugs: Vec<Sug> = Vec::new();
    if !after_table {
        let multi = rts.len() > 1;
        for r in &rts {
            for c in col_matches(r.cols, word) {
                let ambiguous = multi && rts.iter().filter(|o| o.cols.iter().any(|x| x.name.eq_ignore_ascii_case(&c.name))).count() > 1;
                sugs.push(col_sug(scope.dialect, c, &r.table, values, ambiguous.then_some(r.label.as_str()), in_tick));
            }
        }
        if !in_tick {
            let mut fns: Vec<&(&str, &str)> = functions(scope.dialect).iter().filter(|(n, _)| starts_ci(n, word) && !(scope.dialect == Dialect::Postgres && lex::pg_refuses_function(n))).collect();
            fns.sort_by_key(|(n, _)| (n.len(), *n));
            for (name, sigtxt) in fns {
                let ret = sigtxt.split_once(" -> ").map_or("", |x| x.1).to_ascii_lowercase();
                // Functions are always capitals, as in the design; keywords follow what is typed.
                let name = name.to_string();
                // A call with no arguments is complete, one with arguments leaves the caret inside.
                // A keyword form such as CURRENT_DATE is written without a call.
                let (insert, text) = if !sigtxt.contains('(') { (name.clone(), name.clone()) } else if sigtxt.contains("()") { (format!("{name}()"), format!("{name}()")) } else { (format!("{name}("), format!("{name}()")) };
                sugs.push(sug(text, "fn", insert, ret, (*sigtxt).to_string()));
            }
        }
    }
    if !in_tick {
        let mut kws: Vec<&&str> = keywords(scope.dialect).iter().filter(|k| starts_ci(k, word)).collect();
        kws.sort_by_key(|k| (k.len(), **k));
        for k in kws {
            let k = kw_case(word, k);
            sugs.push(sug(k.clone(), "keyword", k, String::new(), String::new()));
        }
    }
    // The word already typed in full, and nothing else to pick, needs no list.
    if sugs.len() == 1 && sugs[0].text.trim_end_matches("()").eq_ignore_ascii_case(word) {
        return None;
    }
    sugs.truncate(MAX_SUGGESTIONS);
    Some(Out { from, sugs, ..Default::default() })
}

/// Columns of the table or alias `q`, or the tables of a database `q`.
fn qualified(q: &str, word: &str, from: usize, in_tick: bool, scope: &Scope, rts: &[Rt], values: &Fields) -> Option<Out> {
    if let Some(r) = by_qualifier(rts, q) {
        return Some(cols_out(scope.dialect, q, r.table.as_str(), r.cols, word, from, in_tick, values));
    }
    if q.eq_ignore_ascii_case(&scope.db) {
        let mut sugs: Vec<Sug> = scope.schema.tables.iter().filter(|t| starts_ci(&t.name, word)).map(|t| table_sug(scope.dialect, t, in_tick)).collect();
        sugs.truncate(MAX_SUGGESTIONS);
        return Some(Out { from, sugs, ..Default::default() });
    }
    let (_, tables) = scope.dbs.iter().find(|(d, _)| d.eq_ignore_ascii_case(q))?;
    let sugs: Vec<Sug> = tables.iter().flatten().filter(|t| starts_ci(t, word)).take(MAX_SUGGESTIONS).map(|t| sug(t.clone(), "table", ident(scope.dialect, t, in_tick), String::new(), String::new())).collect();
    Some(Out { from, sugs, ..Default::default() })
}

fn by_qualifier_cols(table: &str, word: &str, from: usize, in_tick: bool, scope: &Scope, values: &Fields) -> Option<Out> {
    let (name, cols) = table_cols(scope, table)?;
    Some(cols_out(scope.dialect, name, name, cols, word, from, in_tick, values))
}

fn cols_out(dl: Dialect, label: &str, table: &str, cols: &[SchemaCol], word: &str, from: usize, in_tick: bool, values: &Fields) -> Out {
    let sugs: Vec<Sug> = col_matches(cols, word).into_iter().take(MAX_SUGGESTIONS).map(|c| col_sug(dl, c, table, values, None, in_tick)).collect();
    let title = if label.eq_ignore_ascii_case(table) { table.to_ascii_uppercase() } else { format!("{} = {}", label.to_ascii_uppercase(), table.to_ascii_uppercase()) };
    Out { from, sugs, title, ..Default::default() }
}

/// Tables and views of the current database first, then the other databases and their tables.
fn tables_list(word: &str, from: usize, in_tick: bool, scope: &Scope) -> Out {
    let mut sugs: Vec<Sug> = scope.schema.tables.iter().filter(|t| starts_ci(&t.name, word)).map(|t| table_sug(scope.dialect, t, in_tick)).collect();
    // A name still loading is listed from the sidebar's table list, without the row counts.
    if scope.schema.tables.is_empty() && let Some((_, Some(names))) = scope.dbs.iter().find(|(d, _)| d.eq_ignore_ascii_case(&scope.db)) {
        sugs.extend(names.iter().filter(|t| starts_ci(t, word)).map(|t| {
            let view = scope.views.contains(t.as_str());
            sug(t.clone(), if view { "view" } else { "table" }, ident(scope.dialect, t, in_tick), if view { "view".to_string() } else { String::new() }, String::new())
        }));
    }
    for (d, tables) in scope.dbs.iter().filter(|(d, _)| !d.eq_ignore_ascii_case(&scope.db)) {
        if starts_ci(d, word) {
            sugs.push(sug(d.clone(), "schema", format!("{}.", ident(scope.dialect, d, in_tick)), "database".into(), String::new()));
        }
        for t in tables.iter().flatten().filter(|t| starts_ci(t, word)) {
            sugs.push(sug(format!("{d}.{t}"), "schema", format!("{}.{}", ident(scope.dialect, d, false), ident(scope.dialect, t, in_tick)), "other db".into(), String::new()));
        }
    }
    for sch in scope.schemas.iter().filter(|x| !x.eq_ignore_ascii_case(&scope.db) && starts_ci(x, word) && !scope.dbs.iter().any(|(d, _)| d == *x)) {
        sugs.push(sug(sch.clone(), "schema", format!("{}.", ident(scope.dialect, sch, in_tick)), "schema".into(), String::new()));
    }
    sugs.truncate(MAX_SUGGESTIONS);
    let foot = if scope.dbs.iter().any(|(d, _)| !d.eq_ignore_ascii_case(&scope.db)) { format!("Tables and views of {}, then other databases", scope.db) } else { format!("Tables and views of {}", scope.db) };
    Out { from, sugs, foot, ..Default::default() }
}

/// The values of the column whose comparison the open string belongs to. `toks` are the tokens
/// before the string.
fn value_list(toks: &[Tk], typed: &str, from: usize, quote: &str, rts: &[Rt], values: &Fields) -> Option<Out> {
    // The operand ends before `=`, `<>`, `!=`, `LIKE` or an `IN (` list.
    let mut end = toks.len();
    let ops = |t: &Tk| t.tok == Tok::Punct && matches!(t.s, "=" | "<" | ">" | "!");
    if end > 0 && toks[end - 1].tok == Tok::Word && ["LIKE", "REGEXP", "RLIKE"].iter().any(|k| toks[end - 1].is(k)) {
        end -= 1;
    } else if end > 0 && ops(&toks[end - 1]) {
        while end > 0 && ops(&toks[end - 1]) {
            end -= 1;
        }
    } else {
        // Earlier members of an IN list are strings or numbers with commas between them.
        let mut k = end;
        while k >= 2 && toks[k - 1].punct(",") && matches!(toks[k - 2].tok, Tok::Str | Tok::Word) {
            k -= 2;
        }
        if k >= 2 && toks[k - 1].punct("(") && toks[k - 2].is("IN") {
            end = k - 2;
        } else {
            return None;
        }
    }
    if end > 0 && toks[end - 1].is("NOT") {
        end -= 1;
    }
    let parts = operand(toks, end);
    let (table, col) = match parts.as_slice() {
        [c] => rts.iter().find_map(|r| r.cols.iter().find(|x| x.name.eq_ignore_ascii_case(c)).map(|x| (r.table.as_str(), x))),
        [q, c] | [_, q, c] => by_qualifier(rts, q).and_then(|r| r.cols.iter().find(|x| x.name.eq_ignore_ascii_case(c)).map(|x| (r.table.as_str(), x))),
        _ => None,
    }?;
    let numeric = matches!(col.kind, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal | Kind::Year);
    if quote.is_empty() && !numeric {
        return None;
    }
    // A number is written bare, so a quote typed before one is replaced along with the value.
    if numeric {
        let from = from - quote.len();
        let title = format!("{}.{} · {}, MOST FREQUENT", table.to_ascii_uppercase(), col.name.to_ascii_uppercase(), type_label(col).split('(').next().unwrap_or(""));
        let Some(info) = values.get(&value_key(table, &col.name)) else {
            return Some(Out { from, title, need: Some(Need { table: table.to_string(), column: col.name.clone(), prefix: None }), ..Default::default() });
        };
        let sugs: Vec<Sug> = info.values.iter().enumerate().filter_map(|(i, v)| Some((i, v.as_str()?))).filter(|(_, v)| v.starts_with(typed)).take(MAX_SUGGESTIONS).map(|(i, v)| sug(v.to_string(), "literal", v.to_string(), percent(info, i), String::new())).collect();
        let need = (info.values.len() >= 200 && !typed.is_empty()).then(|| Need { table: table.to_string(), column: col.name.clone(), prefix: Some(typed.to_string()) });
        return Some(Out { from, sugs, title, foot: String::new(), need });
    }
    let q = quote.chars().next().unwrap_or('\'');
    // The quote is closed with the value; one doubled stays inside the string. Values with a
    // backslash or a line break are left out, since how they read depends on the server's mode.
    let insert = |v: &str| format!("{}{q}", v.replace(q, &format!("{q}{q}")));
    let usable = |v: &str| !v.contains(['\\', '\n', '\r']);
    let key = value_key(table, &col.name);
    let title = format!("{}.{} · {}", table.to_ascii_uppercase(), col.name.to_ascii_uppercase(), type_label(col).split('(').next().unwrap_or(""));
    let declared = enum_values(&col.ty);
    if !declared.is_empty() {
        let info = values.get(&key);
        let sugs: Vec<Sug> = declared
            .iter()
            .enumerate()
            .filter(|(_, v)| starts_ci(v, typed) && usable(v))
            .map(|(_, v)| {
                let pct = info.and_then(|i| i.values.iter().position(|x| x.as_str() == Some(v)).map(|p| percent(i, p))).unwrap_or_default();
                sug(format!("{q}{v}{q}"), "literal", insert(v), pct, String::new())
            })
            .collect();
        let title = format!("{title}, ALL {} VALUES", declared.len());
        return Some(Out { from, sugs, title, ..Default::default() });
    }
    let Some(info) = values.get(&key) else {
        return Some(Out { from, title, need: Some(Need { table: table.to_string(), column: col.name.clone(), prefix: None }), ..Default::default() });
    };
    let strings: Vec<(usize, &str)> = info.values.iter().enumerate().filter_map(|(i, v)| Some((i, v.as_str()?))).filter(|(_, v)| usable(v)).collect();
    let sugs: Vec<Sug> = strings.iter().filter(|(_, v)| starts_ci(v, typed)).take(MAX_SUGGESTIONS).map(|(i, v)| sug(format!("{q}{v}{q}"), "literal", insert(v), percent(info, *i), String::new())).collect();
    // The list holds the 200 most frequent values; a longer one may leave out what is typed.
    let need = (info.values.len() >= 200 && !typed.is_empty()).then(|| Need { table: table.to_string(), column: col.name.clone(), prefix: Some(typed.to_string()) });
    Some(Out { from, sugs, title: format!("{title}, MOST FREQUENT"), foot: String::new(), need })
}

/// Signature help: the function whose call the caret is in, its parameters and which one is next.
pub(crate) struct Help {
    pub(crate) name: &'static str,
    pub(crate) params: Vec<String>,
    pub(crate) active: usize,
    pub(crate) ret: String,
}

pub(crate) fn sql_help(before: &str, dl: Dialect) -> Option<Help> {
    let probe = format!("{before} ");
    // Inside a string or comment a parenthesis is only text.
    if lex::tokens(&probe, dl).last().is_some_and(|(t, r)| r.start < before.len() && r.end == probe.len() && matches!(t, Tok::Str | Tok::Comment)) {
        return None;
    }
    let toks = lex::tokens(before, dl);
    let t = sig(before, &toks);
    // Walk outward through the open parentheses to the first that belongs to a known function.
    let mut depth = 0;
    let mut commas = vec![0usize];
    for i in (0..t.len()).rev() {
        if t[i].punct(")") {
            depth += 1;
        } else if t[i].punct(",") && depth == 0 {
            *commas.last_mut()? += 1;
        } else if t[i].punct("(") {
            if depth > 0 {
                depth -= 1;
                continue;
            }
            if let Some((name, sigtxt)) = i.checked_sub(1).filter(|_| t[i - 1].tok == Tok::Word).and_then(|k| functions(dl).iter().find(|(n, s)| s.contains('(') && t[k].s.eq_ignore_ascii_case(n))) {
                let inner = sigtxt.split_once('(')?.1;
                let close = inner.rfind(')')?;
                let ret = sigtxt.split_once(" -> ").map_or("", |x| x.1).to_string();
                let params: Vec<String> = inner[..close].split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
                let active = (*commas.last()?).min(params.len().saturating_sub(1));
                return Some(Help { name, params, active, ret });
            }
            commas.push(0);
        }
    }
    None
}

fn help_job(h: &Help) -> LayoutJob {
    let mut job = LayoutJob::default();
    let size = TXT_SMALL;
    job.append(h.name, 0.0, mono_fmt(size, PRIMARY(), W_MEDIUM));
    job.append("(", 0.0, mono_fmt(size, MUTED(), 400.0));
    for (i, p) in h.params.iter().enumerate() {
        if i > 0 {
            job.append(", ", 0.0, mono_fmt(size, MUTED(), 400.0));
        }
        job.append(p, 0.0, if i == h.active { mono_fmt(size, BRIGHT(), W_SEMIBOLD) } else { mono_fmt(size, MUTED(), 400.0) });
    }
    job.append(")", 0.0, mono_fmt(size, MUTED(), 400.0));
    if !h.ret.is_empty() {
        job.append(&format!("  ->  {}", h.ret), 0.0, mono_fmt(size, FAINT(), 400.0));
    }
    job
}

/// A text input with SQL suggestions. `show` draws the TextEdit; the id must be the one it uses.
/// `values` are the looked up column values, keyed by `value_key`. What the caret needs from the
/// server is added to `wants`, which the app sends after the frame. Returns the editor output and
/// whether a suggestion was inserted this frame, so the caller does not also treat Enter as "run".
pub(crate) fn sql_ac_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    ac: &mut Ac,
    scope: &Scope,
    values: &Fields,
    wants: &mut Wants,
    show: impl FnOnce(&mut egui::Ui, &mut String, bool) -> egui::text_edit::TextEditOutput,
) -> (egui::text_edit::TextEditOutput, bool) {
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        wants.schema = true;
    }
    // The text after the caret, from before this frame's edit: the compute step only sees the text before the caret.
    let prev_caret = egui::TextEdit::load_state(ui.ctx(), id).and_then(|s| s.cursor.char_range()).map(|r| usize::from(r.primary.index));
    // Only the tail is copied, and only while focused: the edit below changes `text` before the compute step reads it.
    let tail: String = if focused { prev_caret.map(|c| text.chars().skip(c).collect()).unwrap_or_default() } else { String::new() };
    let meta: RefCell<Option<(String, String)>> = RefCell::new(None);
    let need: RefCell<Option<Need>> = RefCell::new(None);
    let compute = |before: &str| -> Option<(usize, Vec<Sug>)> {
        let (b, full) = split_statement(before, &tail, scope.dialect);
        let out = analyze(&b, &full, scope, values)?;
        // `from` is relative to the statement; the popup wants the typed length.
        *need.borrow_mut() = out.need.clone();
        *meta.borrow_mut() = Some((out.title.clone(), out.foot.clone()));
        (!out.sugs.is_empty()).then(|| (b[out.from..].chars().count(), out.sugs))
    };
    let (out, picked, _moved) = ac_input_with(ui, id, text, ac, &compute, false, show);
    let caret = out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0);
    if ac.id == Some(id) && !ac.items.is_empty() && let Some((t, f)) = meta.borrow_mut().take() {
        (ac.title, ac.foot) = (t, f);
    }
    if let (Some(n), Some(ci)) = (need.borrow_mut().take(), caret) {
        // Values asked for the first time: the list waits, and says so, until they arrive.
        if n.prefix.is_none() && !ac.shown(id) {
            *ac = Ac { id: Some(id), cursor: ci, loading: true, title: format!("{}.{}", n.table, n.column).to_ascii_uppercase(), want: value_key(&n.table, &n.column), ..Default::default() };
        }
        wants.values = Some(n);
    }
    // Values arrived while the list waited: build it now instead of on the next key.
    if ac.id == Some(id) && ac.loading && values.contains_key(&ac.want) {
        let before: String = text.chars().take(ac.cursor).collect();
        let cursor = ac.cursor;
        match compute(&before) {
            Some((token, items)) => {
                *ac = Ac { id: Some(id), token, items, cursor, ..Default::default() };
                if let Some((t, f)) = meta.borrow_mut().take() {
                    (ac.title, ac.foot) = (t, f);
                }
            }
            None => ac.close(),
        }
    }
    if picked || !ac.shown(id) {
        // The call's signature while no list is open, as soon as the caret is past `(` or `,`.
        if let (true, Some(ci)) = (focused && !picked, caret) {
            let before: String = text.chars().take(ci).collect();
            if let Some(h) = sql_help(&before, scope.dialect) {
                let at = out.galley.pos_from_cursor(egui::text::CCursor::new(ci));
                let pos = out.galley_pos + at.left_bottom().to_vec2() + egui::vec2(-6.0, 4.0);
                egui::Area::new(id.with("sql-help")).order(egui::Order::Foreground).fixed_pos(pos).interactable(false).show(ui.ctx(), |ui| {
                    popup_card(ui).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.label(help_job(&h));
                    });
                });
            }
        }
        return (out, picked);
    }
    let at = out.galley.pos_from_cursor(egui::text::CCursor::new(ac.cursor));
    let pos = out.galley_pos + at.left_bottom().to_vec2() + egui::vec2(-6.0, 4.0);
    let mut picked = false;
    egui::Area::new(id.with("ac")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        picked = super::complete::ac_list(ui, id, text, ac, false, None);
    });
    (out, picked)
}

/// Opens the list as if `text` had just been typed with the caret at its end.
pub(crate) fn open_list(ui: &egui::Ui, id: egui::Id, text: &str, ac: &mut Ac, scope: &Scope, values: &Fields) {
    if ac.id.is_some() {
        return;
    }
    let (b, full) = split_statement(text, "", scope.dialect);
    if let Some(out) = analyze(&b, &full, scope, values).filter(|o| !o.sugs.is_empty()) {
        let n = text.chars().count();
        *ac = Ac { id: Some(id), token: b[out.from..].chars().count(), items: out.sugs, cursor: n, title: out.title, foot: out.foot, ..Default::default() };
        ui.memory_mut(|m| m.request_focus(id));
        let mut st = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(n))));
        st.store(ui.ctx(), id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggest(before: &str, full_stmt: &str, scope: &Scope, values: &Fields) -> Option<(usize, Vec<Sug>)> {
        let out = analyze(before, full_stmt, scope, values)?;
        (!out.sugs.is_empty()).then_some((out.from, out.sugs))
    }
    use mongodb::bson::Bson;

    fn col(name: &str, ty: &str, key: &str, fk: bool) -> SchemaCol {
        let base: String = ty.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        SchemaCol { name: name.into(), ty: ty.into(), kind: Kind::from_type_name(&base), key: key.into(), nullable: false, fk }
    }

    fn scope() -> Scope<'static> {
        let orders = SchemaTable {
            name: "orders".into(),
            view: false,
            rows: Some(4410),
            cols: vec![
                col("id", "int unsigned", "PRI", false),
                col("customer_id", "int", "MUL", true),
                col("status", "enum('paid','shipped','it''s')", "", false),
                col("currency", "char(3)", "", false),
                col("created_at", "datetime", "", false),
            ],
        };
        let customers = SchemaTable { name: "customers".into(), view: false, rows: Some(1284), cols: vec![col("id", "int unsigned", "PRI", false), col("email", "varchar(255)", "UNI", false), col("name", "varchar(120)", "", false)] };
        let items = SchemaTable { name: "order_items".into(), view: false, rows: Some(8902), cols: vec![col("order_id", "int", "MUL", true)] };
        let totals = SchemaTable { name: "order_totals".into(), view: true, rows: None, cols: vec![col("total", "decimal(10,2)", "", false)] };
        let weird = SchemaTable { name: "order".into(), view: false, rows: None, cols: vec![col("my col", "int", "", false)] };
        Scope {
            schema: Arc::new(Schema { tables: vec![customers, items, orders, totals, weird] }),
            db: "shop".into(),
            dbs: Box::leak(vec![("analytics".into(), Some(vec!["orders_daily".into(), "visits".into()])), ("shop".into(), Some(vec![]))].into_boxed_slice()),
            ..Default::default()
        }
    }

    fn text_of(s: &[Sug]) -> Vec<&str> {
        s.iter().map(|x| x.text.as_str()).collect()
    }

    fn run(sql: &str) -> Option<(usize, Vec<Sug>)> {
        run_with(sql, &Fields::new())
    }

    /// The caret is where the `|` stands.
    fn run_with(sql: &str, values: &Fields) -> Option<(usize, Vec<Sug>)> {
        let (before, tail) = sql.split_once('|').unwrap();
        let (b, full) = split_statement(before, tail, Dialect::MySql);
        suggest(&b, &full, &scope(), values).map(|(from, s)| (from + (before.len() - b.len()), s))
    }

    fn values() -> Fields {
        let mut f = Fields::new();
        f.insert(value_key("orders", "currency"), super::super::complete::FieldInfo { kind: "string", values: ["EUR", "USD", "GBP"].map(Bson::from).to_vec(), freq: vec![61, 27, 12] });
        f
    }

    #[test]
    fn tables_after_from_current_database_first() {
        let (from, s) = run("SELECT * FROM or|").unwrap();
        assert_eq!(from, "SELECT * FROM ".len());
        assert_eq!(text_of(&s), ["order_items", "orders", "order_totals", "order", "analytics.orders_daily"]);
        let d = |i: usize| s[i].sql.as_ref().unwrap().detail.as_str();
        assert_eq!((s[0].note, d(0)), ("table", "8,902 rows"));
        assert_eq!((s[1].note, d(1)), ("table", "4,410 rows"));
        assert_eq!((s[2].note, d(2)), ("view", "view"));
        assert_eq!((s[4].note, d(4)), ("schema", "other db"));
        // A name that needs backticks gets them.
        assert_eq!(s[3].insert, "`order`");
    }

    #[test]
    fn tables_after_join_update_into_and_comma() {
        for sql in ["SELECT 1 FROM orders o JOIN cu|", "UPDATE cu|", "INSERT INTO cu|", "SELECT 1 FROM orders o, cu|"] {
            let (_, s) = run(sql).unwrap_or_else(|| panic!("{sql}"));
            assert_eq!(s[0].text, "customers", "{sql}");
        }
        // A comma in a SELECT list is not a table list.
        let (_, s) = run("SELECT id, em| FROM customers").unwrap();
        assert_eq!(s[0].note, "column");
    }

    #[test]
    fn tables_of_another_database_and_its_name() {
        let (_, s) = run("SELECT * FROM analytics.or|").unwrap();
        assert_eq!(text_of(&s), ["orders_daily"]);
        let (_, s) = run("SELECT * FROM an|").unwrap();
        assert_eq!((s[0].text.as_str(), s[0].insert.as_str()), ("analytics", "analytics."));
        let (_, s) = run("SELECT * FROM shop.cu|").unwrap();
        assert_eq!(text_of(&s), ["customers"]);
    }

    #[test]
    fn columns_after_an_alias_with_types_and_keys() {
        let (from, s) = run("SELECT o.id, c.name FROM orders o\nJOIN customers c ON c.|").unwrap();
        assert_eq!(from, "SELECT o.id, c.name FROM orders o\nJOIN customers c ON c.".len());
        assert_eq!(text_of(&s), ["id", "email", "name"]);
        let d = |i: usize| s[i].sql.as_ref().unwrap().detail.as_str();
        assert_eq!((s[0].note, d(0)), ("pk", "INT UNSIGNED"));
        assert_eq!(d(1), "VARCHAR(255) · UNIQUE");
        // Aliases are read from the part of the statement after the caret too.
        let (_, s) = run("SELECT o.cu| FROM orders AS o").unwrap();
        assert_eq!(text_of(&s), ["customer_id", "currency"]);
        assert_eq!(s[0].sql.as_ref().unwrap().detail, "INT · FK");
        // The table's own name works as a qualifier, and the typed prefix filters.
        let (_, s) = run("SELECT customers.e| FROM customers").unwrap();
        assert_eq!(text_of(&s), ["email"]);
    }

    #[test]
    fn column_list_is_titled_with_the_alias() {
        let sc = scope();
        let o = analyze("SELECT * FROM customers c WHERE c.", "SELECT * FROM customers c WHERE c.", &sc, &Fields::new()).unwrap();
        assert_eq!(o.title, "C = CUSTOMERS");
        let o = analyze("SELECT customers.", "SELECT customers. FROM customers", &sc, &Fields::new()).unwrap();
        assert_eq!(o.title, "CUSTOMERS");
    }

    #[test]
    fn columns_after_database_and_table() {
        let (_, s) = run("SELECT shop.customers.e| FROM shop.customers").unwrap();
        assert_eq!(text_of(&s), ["email"]);
        // A database name lists its tables.
        let (_, s) = run("SELECT * FROM shop.|").unwrap();
        assert!(text_of(&s).contains(&"customers"));
        let (_, s) = run("SELECT analytics.v| FROM x").unwrap();
        assert_eq!(text_of(&s), ["visits"]);
        assert!(run("SELECT nothing.| FROM x").is_none());
    }

    #[test]
    fn backtick_quoted_names() {
        // An open backtick is closed by the insert, and the quoted alias still resolves.
        let (from, s) = run("SELECT * FROM `or|").unwrap();
        assert_eq!(from, "SELECT * FROM `".len());
        assert_eq!(s[0].insert, "order_items`");
        let (_, s) = run("SELECT `o`.`cu| FROM orders `o`").unwrap();
        assert_eq!(text_of(&s), ["customer_id", "currency"]);
        assert_eq!(s[0].insert, "customer_id`");
        // A column with a space is quoted when it is not already in backticks.
        let (_, s) = run("SELECT order.m| FROM `order`").unwrap_or((0, Vec::new()));
        assert!(s.is_empty() || s[0].insert == "`my col`");
        let (_, s) = run("SELECT m| FROM `order`").unwrap();
        assert_eq!(s[0].insert, "`my col`");
        // A closed identifier has nothing to complete.
        assert!(run("SELECT * FROM `orders`|").is_none());
    }

    #[test]
    fn values_of_an_enum_without_a_lookup() {
        let (from, s) = run("SELECT * FROM orders o WHERE o.status = '|").unwrap();
        assert_eq!(from, "SELECT * FROM orders o WHERE o.status = '".len());
        assert_eq!(text_of(&s), ["'paid'", "'shipped'", "'it's'"]);
        assert_eq!(s[0].insert, "paid'");
        // A quote inside a value is doubled.
        assert_eq!(s[2].insert, "it''s'");
        let o = analyze("SELECT * FROM orders o WHERE o.status = 'sh", "SELECT * FROM orders o WHERE o.status = 'sh", &scope(), &Fields::new()).unwrap();
        assert_eq!(text_of(&o.sugs), ["'shipped'"]);
        assert_eq!(o.title, "ORDERS.STATUS · ENUM, ALL 3 VALUES");
        assert!(o.need.is_none());
    }

    #[test]
    fn numeric_values_go_in_bare() {
        let mut v = Fields::new();
        v.insert(value_key("orders", "customer_id"), super::super::complete::FieldInfo { kind: "string", values: ["12", "5", "120"].map(Bson::from).to_vec(), freq: vec![5, 3, 2] });
        // A quote typed first is replaced, since a number needs none.
        let (from, s) = run_with("SELECT * FROM orders WHERE customer_id = \"1|", &v).unwrap();
        assert_eq!(from, "SELECT * FROM orders WHERE customer_id = ".len());
        assert_eq!(text_of(&s), ["12", "120"]);
        assert_eq!(s[0].insert, "12");
        let (from, s) = run_with("SELECT * FROM orders WHERE customer_id IN (5, 1|", &v).unwrap();
        assert_eq!(from, "SELECT * FROM orders WHERE customer_id IN (5, ".len());
        assert_eq!(text_of(&s), ["12", "120"]);
        // A digit after a text column is no value.
        assert!(run_with("SELECT * FROM orders WHERE currency = 1|", &values()).is_none_or(|(_, s)| s.iter().all(|s| s.note != "literal")));
    }

    #[test]
    fn values_after_each_comparison() {
        let v = values();
        for sql in [
            "SELECT * FROM orders WHERE currency = '|",
            "SELECT * FROM orders WHERE currency <> '|",
            "SELECT * FROM orders WHERE currency != '|",
            "SELECT * FROM orders WHERE currency LIKE '|",
            "SELECT * FROM orders WHERE currency NOT LIKE '|",
            "SELECT * FROM orders WHERE currency IN ('|",
            "SELECT * FROM orders WHERE currency IN ('EUR', '|",
            "SELECT * FROM orders WHERE orders.currency NOT IN ('EUR','|",
        ] {
            let (_, s) = run_with(sql, &v).unwrap_or_else(|| panic!("{sql}"));
            assert_eq!(text_of(&s), ["'EUR'", "'USD'", "'GBP'"], "{sql}");
        }
        let (_, s) = run_with("SELECT * FROM orders WHERE currency = '|", &v).unwrap();
        let notes: Vec<&str> = s.iter().map(|x| x.sql.as_ref().unwrap().detail.as_str()).collect();
        assert_eq!(notes, ["61%", "27%", "12%"]);
        // A typed prefix filters, and the quote style is kept.
        let (_, s) = run_with("SELECT * FROM orders WHERE currency = \"U|", &v).unwrap();
        assert_eq!((text_of(&s), s[0].insert.as_str()), (vec!["\"USD\""], "USD\""));
    }

    #[test]
    fn values_that_are_not_known_yet_are_asked_for() {
        let o = analyze("SELECT * FROM orders WHERE currency = '", "SELECT * FROM orders WHERE currency = '", &scope(), &Fields::new()).unwrap();
        assert!(o.sugs.is_empty());
        assert_eq!(o.need, Some(Need { table: "orders".into(), column: "currency".into(), prefix: None }));
        // With 200 values known, typed text may be missing from them and is asked for.
        let mut f = Fields::new();
        f.insert(value_key("orders", "currency"), super::super::complete::FieldInfo { kind: "string", values: (0..200).map(|i| Bson::from(format!("v{i}"))).collect(), freq: Vec::new() });
        let o = analyze("SELECT * FROM orders WHERE currency = 'zz", "SELECT * FROM orders WHERE currency = 'zz", &scope(), &f).unwrap();
        assert_eq!(o.need.unwrap().prefix.as_deref(), Some("zz"));
    }

    #[test]
    fn no_values_in_other_strings() {
        let v = values();
        // Not after a comparison, in a function's string, or for an unknown column.
        assert!(run_with("SELECT 'a|", &v).is_none());
        assert!(run_with("SELECT * FROM orders WHERE note = 'x' AND CONCAT('|", &v).is_none());
        assert!(run_with("SELECT * FROM orders WHERE nothing = '|", &v).is_none());
        assert!(run_with("SELECT * FROM orders WHERE LOWER(currency) = '|", &v).is_none());
    }

    #[test]
    fn nothing_inside_comments_or_after_a_finished_string() {
        for sql in ["SELECT 1 -- FROM or|", "# FROM or|", "/* FROM or|", "SELECT '' /* x */ -- cu|"] {
            assert!(run(sql).is_none(), "{sql}");
        }
        // A comment that has ended does not hide the next line.
        assert!(run("-- note\nSELECT * FROM or|").is_some());
        assert!(run("/* note */ SELECT * FROM or|").is_some());
        // A finished string is not a place for suggestions either.
        assert!(run("SELECT * FROM orders WHERE id IN ('a')|").is_none());
    }

    #[test]
    fn columns_functions_keywords_in_that_order_with_case_kept() {
        let (_, s) = run("SELECT * FROM orders WHERE c|").unwrap();
        let kinds: Vec<&str> = s.iter().map(|x| x.note).collect();
        let first_fn = kinds.iter().position(|k| *k == "fn").unwrap();
        let first_kw = kinds.iter().position(|k| *k == "keyword").unwrap();
        assert!(kinds[..first_fn].iter().all(|k| *k == "column" || *k == "pk") && first_fn < first_kw);
        assert_eq!(text_of(&s)[..2], ["customer_id", "currency"]);
        // Functions show a call and what they return; zero-argument ones are closed.
        let cur = s.iter().find(|x| x.text == "CURDATE()").unwrap();
        assert_eq!((cur.insert.as_str(), cur.sql.as_ref().unwrap().detail.as_str()), ("CURDATE()", "date"));
        let cnt = run("SELECT cou|").unwrap().1;
        let count = cnt.iter().find(|x| x.note == "fn").unwrap();
        assert_eq!((count.text.as_str(), count.insert.as_str()), ("COUNT()", "COUNT("));
        // Typed capitals give capital keywords, lower case gives lower case; functions stay capitals.
        let upper = run("SELECT * FROM orders WHERE ORD|").unwrap().1;
        assert!(upper.iter().any(|x| x.text == "ORDER"));
        let lower = run("select * from orders where ord|").unwrap().1;
        assert!(lower.iter().any(|x| x.text == "order"));
    }

    #[test]
    fn the_content_bar_has_its_table_without_a_from() {
        let mut sc = scope();
        sc.table = Some("orders".into());
        let sug = |text: &str| {
            let (b, full) = split_statement(text, "", Dialect::MySql);
            suggest(&b, &full, &sc, &values()).map(|x| x.1)
        };
        let s = sug("status = 'paid' AND cur").unwrap();
        assert_eq!(text_of(&s)[..4], ["currency", "customer_id", "CURDATE()", "CURTIME()"]);
        assert_eq!(s[0].sql.as_ref().unwrap().detail, "CHAR(3)");
        assert_eq!(s[0].sql.as_ref().unwrap().foot, "Values: 'EUR' 61%  'USD' 27%  'GBP' 12%");
        // Before the schema arrives the bar's own columns serve.
        sc.schema = Arc::new(Schema::default());
        sc.fallback = Arc::new(vec![col("currency", "char(3)", "", false)]);
        let (b, full) = split_statement("cur", "", Dialect::MySql);
        assert_eq!(text_of(&suggest(&b, &full, &sc, &Fields::new()).unwrap().1)[0], "currency");
        // Values work from the fallback too.
        let (b, full) = split_statement("currency = '", "", Dialect::MySql);
        assert!(analyze(&b, &full, &sc, &Fields::new()).unwrap().need.is_some());
    }

    #[test]
    fn several_tables_qualify_a_shared_column() {
        let (_, s) = run("SELECT i| FROM orders o JOIN customers c ON 1").unwrap();
        assert_eq!(text_of(&s)[..2], ["o.id", "c.id"]);
        assert_eq!(s[0].insert, "o.id");
        // A column of one table only stays plain.
        let (_, s) = run("SELECT em| FROM orders o JOIN customers c ON 1").unwrap();
        assert_eq!(s[0].insert, "email");
    }

    #[test]
    fn insert_column_list_and_update_set() {
        let (_, s) = run("INSERT INTO customers (em|").unwrap();
        assert_eq!(s[0].text, "email");
        let (_, s) = run("UPDATE customers SET na|").unwrap();
        assert_eq!(s[0].text, "name");
    }

    #[test]
    fn only_keywords_where_an_alias_goes() {
        let (_, s) = run("SELECT * FROM orders wh|").unwrap();
        assert!(s.iter().all(|x| x.note == "keyword"));
        assert!(s.iter().any(|x| x.text == "where"));
        assert!(run("SELECT * FROM orders AS x|").is_none());
    }

    #[test]
    fn a_word_typed_in_full_needs_no_list_and_empty_words_stay_quiet() {
        assert!(run("SELECT * FROM orders WHERE statu|").is_some());
        assert!(run("SELECT * FROM orders WHERE JOIN|").is_none());
        assert!(run("SELECT * FROM |").is_none());
        assert!(run("SELECT |").is_none());
        assert!(run("|").is_none());
    }

    #[test]
    fn statements_are_split_outside_strings() {
        let (b, full) = split_statement("SELECT 1; SELECT * FROM or", "ders; SELECT 3", Dialect::MySql);
        assert_eq!((b.trim(), full.trim()), ("SELECT * FROM or", "SELECT * FROM orders"));
        let (b, full) = split_statement("SELECT ';' FROM o", "", Dialect::MySql);
        assert_eq!((b.as_str(), full.as_str()), ("SELECT ';' FROM o", "SELECT ';' FROM o"));
        // Suggestions in the second statement do not see the first one's aliases.
        assert!(run("SELECT 1 FROM orders x; SELECT x.|").is_none());
    }

    #[test]
    fn signature_help_names_the_current_argument() {
        let h = sql_help("SELECT DATE_FORMAT(o.created_at, ", Dialect::MySql).unwrap();
        assert_eq!((h.name, h.active, h.ret.as_str()), ("DATE_FORMAT", 1, "VARCHAR"));
        assert_eq!(h.params, ["date", "format"]);
        assert_eq!(sql_help("SELECT date_format(", Dialect::MySql).unwrap().active, 0);
        // A nested call that is finished does not count, an inner call wins, a list is not a call.
        assert_eq!(sql_help("SELECT CONCAT(LOWER(a), ", Dialect::MySql).unwrap().active, 1);
        assert_eq!(sql_help("SELECT CONCAT(LOWER(a, ", Dialect::MySql).unwrap().name, "LOWER");
        assert_eq!(sql_help("SELECT COALESCE(a IN (1, 2, ", Dialect::MySql).map(|h| h.name), Some("COALESCE"));
        assert!(sql_help("SELECT 1 WHERE a IN (1, ", Dialect::MySql).is_none());
        // Closed calls, strings and comments show nothing; a long argument list stays on the last parameter.
        assert!(sql_help("SELECT NOW() ", Dialect::MySql).is_none());
        assert!(sql_help("SELECT 'DATE_FORMAT(", Dialect::MySql).is_none());
        assert!(sql_help("-- DATE_FORMAT(", Dialect::MySql).is_none());
        assert_eq!(sql_help("SELECT CONCAT(a, b, c, d, ", Dialect::MySql).unwrap().active, 2);
    }

    fn pg_scope() -> Scope<'static> {
        let users = SchemaTable { name: "users".into(), view: false, rows: Some(10), cols: vec![col("id", "int4", "PRI", false), col("email", "text", "", false), col("Full Name", "text", "", false)] };
        let order = SchemaTable { name: "order".into(), view: false, rows: None, cols: vec![col("total", "numeric", "", false)] };
        Scope {
            schema: Arc::new(Schema { tables: vec![users, order] }),
            db: "public".into(),
            dbs: Box::leak(vec![("sales".into(), Some(vec!["invoices".into()])), ("public".into(), Some(vec![]))].into_boxed_slice()),
            dialect: Dialect::Postgres,
            schemas: vec!["audit".into()],
            ..Default::default()
        }
    }

    fn run_pg(sql: &str) -> Option<(usize, Vec<Sug>)> {
        let (before, tail) = sql.split_once('|').unwrap();
        let sc = pg_scope();
        let (b, full) = split_statement(before, tail, sc.dialect);
        suggest(&b, &full, &sc, &Fields::new()).map(|(from, s)| (from + (before.len() - b.len()), s))
    }

    #[test]
    fn keywords_and_functions_follow_the_dialect() {
        let (_, pg) = run_pg("SELECT * FROM users WHERE a IL|").unwrap();
        assert_eq!(text_of(&pg), ["ILIKE"]);
        assert!(run("SELECT * FROM orders WHERE a IL|").is_none());
        let (_, pg) = run_pg("SELECT gen_r|").unwrap();
        assert_eq!(text_of(&pg), ["GEN_RANDOM_UUID()"]);
        assert!(run("SELECT gen_r|").is_none());
        let (_, my) = run("SELECT date_for|").unwrap();
        assert_eq!(text_of(&my), ["DATE_FORMAT()"]);
        assert!(run_pg("SELECT date_for|").is_none());
        let (_, pg) = run_pg("SELECT * FROM users RETU|").unwrap();
        assert!(text_of(&pg).contains(&"RETURNING"));
        let (_, pg) = run_pg("SELECT curr|").unwrap();
        let cd = pg.iter().find(|s| s.text == "CURRENT_DATE").unwrap();
        assert_eq!(cd.insert, "CURRENT_DATE");
    }

    #[test]
    fn types_follow_a_double_colon() {
        let (from, s) = run_pg("SELECT id::|").unwrap();
        assert_eq!(from, "SELECT id::".len());
        assert!(text_of(&s).contains(&"jsonb") && text_of(&s).contains(&"double precision") && text_of(&s).contains(&"text[]"));
        let (_, s) = run_pg("SELECT id::ti|").unwrap();
        assert_eq!(text_of(&s), ["timestamp", "timestamptz", "time"]);
        assert_eq!(s[0].note, "type");
        // A single colon is not a cast, and MySQL has no casts of this kind.
        assert!(run_pg("SELECT id:|").is_none());
        assert!(run("SELECT id::|").is_none());
    }

    #[test]
    fn postgres_quotes_what_it_would_fold_or_reject() {
        let pg = Dialect::Postgres;
        for name in ["order", "User", "full name", "1abc", "a-b", "", "select", "caf\u{e9}"] {
            assert!(needs_quote(pg, name), "{name}");
        }
        for name in ["users", "order_items", "col1", "_x"] {
            assert!(!needs_quote(pg, name), "{name}");
        }
        assert_eq!(ident(pg, "order", false), "\"order\"");
        assert_eq!(ident(pg, "Full Name", false), "\"Full Name\"");
        assert_eq!(ident(pg, "a\"b", false), "\"a\"\"b\"");
        assert_eq!(ident(pg, "users", false), "users");
        assert_eq!(ident(pg, "users", true), "users\"");
        // MySQL keeps its rules: mixed case is fine, backticks are used.
        assert!(!needs_quote(Dialect::MySql, "Users"));
        assert_eq!(ident(Dialect::MySql, "order", false), "`order`");
        let (_, s) = run_pg("SELECT * FROM or|").unwrap();
        assert_eq!(s[0].insert, "\"order\"");
        let (_, s) = run_pg("SELECT fu| FROM users").unwrap();
        assert_eq!(s[0].insert, "\"Full Name\"");
    }

    #[test]
    fn schema_qualified_tables_and_columns() {
        let (_, s) = run_pg("SELECT * FROM sales.in|").unwrap();
        assert_eq!(text_of(&s), ["invoices"]);
        let (_, s) = run_pg("SELECT * FROM public.us|").unwrap();
        assert_eq!(text_of(&s), ["users"]);
        let (_, s) = run_pg("SELECT public.users.| FROM public.users").unwrap();
        assert!(text_of(&s).contains(&"email"));
        let (_, s) = run_pg("SELECT public.users.em| FROM public.users").unwrap();
        assert_eq!(text_of(&s), ["email"]);
        let (_, s) = run_pg("SELECT u.em| FROM public.users u").unwrap();
        assert_eq!(text_of(&s), ["email"]);
        // The schemas the app lists complete before their tables are known.
        let (_, s) = run_pg("SELECT * FROM au|").unwrap();
        assert_eq!((s[0].text.as_str(), s[0].insert.as_str()), ("audit", "audit."));
        // Unknown schemas offer nothing and do not panic.
        assert!(run_pg("SELECT * FROM nope.|").is_none());
        assert!(run_pg("SELECT nope.users.|").is_none());
        assert!(run_pg("SELECT * FROM audit.|").is_none());
        assert!(run_pg("SELECT a.b.c.d.|").is_none());
    }

    #[test]
    fn dollar_quotes_and_escape_strings_do_not_break_completion() {
        let (_, s) = run_pg("DO $$ BEGIN PERFORM 1; END $$; SELECT * FROM us|").unwrap();
        assert_eq!(text_of(&s), ["users"]);
        let (_, s) = run_pg("SELECT $tag$ a; b $tag$, em| FROM users").unwrap();
        assert_eq!(text_of(&s), ["email"]);
        let (_, s) = run_pg(r"SELECT E'it\'s; here', em| FROM users").unwrap();
        assert_eq!(text_of(&s), ["email"]);
        // Inside the body there is nothing to offer.
        assert!(run_pg("SELECT $$ FROM us| $$").is_none());
        let (b, full) = split_statement("SELECT $$;$$; SELECT 2 FROM us", "ers; SELECT 3", Dialect::Postgres);
        assert_eq!((b.as_str(), full.as_str()), (" SELECT 2 FROM us", " SELECT 2 FROM users"));
    }

    #[test]
    fn postgres_signature_help() {
        let h = sql_help("SELECT date_trunc('day', ", Dialect::Postgres).unwrap();
        assert_eq!((h.name, h.active, h.params.len()), ("DATE_TRUNC", 1, 2));
        assert_eq!(sql_help("SELECT CONCAT_WS(a, $$x,y$$, ", Dialect::Postgres).unwrap().active, 2);
        assert!(sql_help("SELECT $$ date_trunc(", Dialect::Postgres).is_none());
        assert!(sql_help("SELECT date_trunc(", Dialect::MySql).is_none());
        assert!(sql_help("SELECT CURRENT_DATE (", Dialect::Postgres).is_none());
    }

    #[test]
    fn no_refused_function_is_ever_offered() {
        for (name, _) in FUNCTIONS_PG {
            assert!(!lex::pg_refuses_function(name), "{name}");
        }
        for pre in ["n", "pg_", "lo_", "set", "db", "txid", "cross", "pg_ad", "next"] {
            let (_, s) = run_pg(&format!("SELECT {pre}|")).unwrap_or((0, vec![]));
            for x in s.iter().filter(|x| x.note == "fn") {
                assert!(!lex::pg_refuses_function(x.text.trim_end_matches("()")), "{}", x.text);
            }
        }
    }
}
