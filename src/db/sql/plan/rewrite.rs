//! Filters written so an index the table already has cannot serve them, and what would: the same condition rewritten
//! (a date or a year turned into a range, a number compared as text turned back into a number), an index on the
//! expression the filter tests, a trigram index for a pattern that starts with a wildcard, an index for each side of an
//! OR. Each needs the catalog to show a valid B-tree index leading with the column the filter defeats, so a table
//! without one gets nothing here. Also the column a MySQL or MariaDB join buffer matches on, when no index leads with it.
//! Precision over recall: a rewrite is offered only when it returns the same rows, and an index only when the table is
//! big, its statistics fresh and the filter measured or estimated to keep few rows, as for any candidate index.

use super::super::lex::{self, Tok};
use super::cond::{self, ColRef, Expr, SargKind, Term};
use super::evidence::{reads_all, Evidence, TableEv};
use super::findings::{and_list, pct_text, weight_of, Finding, Fix, FixKind, Kind, Severity};
use super::index::{self, How, Kept, MAX_KEPT, MIN_ROWS};
use super::{count, version_of, Dialect, Plan, PlanNode};
use chrono::NaiveDate;
use std::ops::Range;

const NOTE: &str = "Check it against the table's other queries before creating it.";
/// A trigram index narrows only with this many characters in a row that are not wildcards.
const TRGM_RUN: usize = 3;

/// What one condition does to the index on its column, and what is offered for it.
struct Spot {
    index: String,
    body: Vec<String>,
    fix: Option<Fix>,
    no_fix: Option<String>,
    lines: Vec<String>,
}

/// Adds what the catalog shows about filters that defeat an index, to the reads that are already a finding.
pub(super) fn add(p: &Plan, d: Dialect, ev: &Evidence, pooler: bool, by_time: bool, out: &mut Vec<Finding>) {
    for (i, n) in p.nodes.iter().enumerate() {
        let flagged = out.iter().any(|f| f.node == Some(i) && matches!(f.kind, Kind::FullScan | Kind::FullIndexScan | Kind::UnusedIndex | Kind::Misestimate));
        if !reads_all(n) || !flagged {
            continue;
        }
        let (Some(t), Some(e)) = (ev.table(n), &n.filter.expr) else { continue };
        // A table this small reads whole about as fast as through an index, rewritten or not.
        if !index::table_rows(p, i, t, d).is_some_and(|r| r >= MIN_ROWS) {
            continue;
        }
        if let Some(s) = spot(p, i, d, ev, t, e, pooler) {
            place(p, i, d, s, by_time, out);
        }
    }
    join_buffers(p, d, ev, pooler, out);
}

/// The top-level conditions of a filter.
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::And(xs) => xs.iter().collect(),
        x => vec![x],
    }
}

/// The first condition of the filter that defeats an index leading with its column.
fn spot(p: &Plan, i: usize, d: Dialect, ev: &Evidence, t: &TableEv, e: &Expr, pooler: bool) -> Option<Spot> {
    let n = &p.nodes[i];
    let own = |c: &ColRef| c.qual.as_deref().is_none_or(|q| index::same(q, &n.alias, d) || index::same(q, &n.rel, d) || index::same(q, &t.table, d));
    for x in conjuncts(e) {
        let found = match shape(x, &own) {
            Some(s) if s.form == Form::Text => text_cast(p, i, d, t, &s),
            Some(s) => dated(p, i, d, t, &s),
            None => folded(p, i, d, ev, t, x, &own, pooler).or_else(|| wild(p, i, d, ev, t, x, &own, pooler)).or_else(|| either(p, i, d, t, x, &own, pooler)),
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// The finding for `s`, in place of the vaguer ones about the same index: MySQL's possible keys and the advice that only
/// named the blocker. It goes before the node's full scan finding, since it says what to do about it.
fn place(p: &Plan, i: usize, d: Dialect, s: Spot, by_time: bool, out: &mut Vec<Finding>) {
    let n = &p.nodes[i];
    out.retain(|f| !(f.node == Some(i) && f.kind == Kind::UnusedIndex));
    // The index advice reads a blocked column's index as one that serves the filter; it does once the filter changes.
    let serves = format!("No index suggested: {} already serves this filter.", s.index);
    for f in out.iter_mut().filter(|f| f.node == Some(i) && f.no_fix.as_deref() == Some(serves.as_str())) {
        f.no_fix = Some(format!("No index suggested: {} would serve this filter, but the filter is written so it cannot.", s.index));
    }
    let who = if d == Dialect::Postgres { "planner" } else { "server" };
    let at = out.iter().position(|f| f.node == Some(i)).unwrap_or(out.len());
    out.insert(at, Finding { kind: Kind::UnusedIndex, severity: Severity::Warn, node: Some(i), title: format!("{} has an index the {who} did not use", n.name()), weight: weight_of(n, by_time), share: n.share.unwrap_or(0.0), body: s.body, fix: s.fix, evidence: s.lines, no_fix: s.no_fix });
}

/// A valid, plain B-tree index whose first column is `col`.
fn lead(t: &TableEv, col: &str, d: Dialect) -> Option<String> {
    t.indexes.as_ref()?.iter().find(|ix| index::usable(ix) && ix.cols.first().is_some_and(|c| index::same(c, col, d))).map(|ix| ix.name.clone())
}

/// The table's size and statistics, which every finding here shows.
fn base_lines(p: &Plan, i: usize, d: Dialect, t: &TableEv) -> Vec<String> {
    let mut lines = vec![index::size_line(t, index::table_rows(p, i, t, d))];
    lines.extend(index::stats_line(t));
    lines
}

/// The gates of any candidate index: fresh statistics and a filter measured or estimated to keep few rows. The lines
/// are the evidence either way; the reason is why no index is offered, when one is not.
fn gates(p: &Plan, i: usize, d: Dialect, t: &TableEv) -> (Vec<String>, Option<String>) {
    let mut lines = base_lines(p, i, d, t);
    if let Some(why) = index::stale(t) {
        return (lines, Some(format!("{why}. Refresh the statistics first")));
    }
    let (f, how) = match index::kept(p, i, t, d) {
        Kept::Guess(f) => return (lines, Some(format!("the {} of rows the server says the filter keeps is its guess. Run with ANALYZE to measure it", pct_text(f * 100.0)))),
        Kept::Unknown => return (lines, Some("how many rows the filter keeps is not known".into())),
        Kept::Measured(f) => (f, "measured"),
        Kept::Estimated(f) => (f, "by the planner's estimate"),
    };
    lines.push(format!("The filter keeps {} of the rows it reads, {how}.", pct_text(f * 100.0)));
    let why = (f > MAX_KEPT).then(|| format!("the filter keeps {} of the rows, too many for an index to read less than the scan", pct_text(f * 100.0)));
    (lines, why)
}

/// An index name past those the table has.
fn name_for(d: Dialect, t: &TableEv, parts: &[String], also: &[String]) -> String {
    let mut taken: Vec<&str> = t.indexes.iter().flatten().map(|ix| ix.name.as_str()).collect();
    taken.extend(also.iter().map(String::as_str));
    index::index_name(&t.table, parts, &taken, d.max_ident(), d)
}

/// A name as SQL: bare when it reads as a plain identifier, else quoted.
fn ident_text(s: &str, d: Dialect) -> String {
    let plain = s.chars().next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic()) && s.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) && (d != Dialect::Postgres || !s.chars().any(|c| c.is_ascii_uppercase()));
    // A column named like a keyword (order, user, key) breaks the statement unless quoted; quoting any of them is always valid.
    let keyword = super::super::dialect::KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(s));
    if plain && !keyword { s.to_string() } else { d.quote_ident(s) }
}

fn col_text(c: &ColRef, d: Dialect) -> String {
    match &c.qual {
        Some(q) => format!("{}.{}", ident_text(q, d), ident_text(&c.name, d)),
        None => ident_text(&c.name, d),
    }
}

// Dates, years and numbers compared as text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// `(col)::date`, `date(col)`, MySQL's `cast(col as date)`.
    Date,
    /// MySQL's `year(col)`.
    Year,
    /// `(col)::text`, `cast(col as char)`.
    Text,
}

/// A comparison of a converted column with a value, read with the column on the left.
struct Shape<'a> {
    col: &'a ColRef,
    form: Form,
    op: String,
    /// The literal compared with, when it is one this rewrite can work with.
    lit: Option<String>,
}

fn shape<'a>(x: &'a Expr, own: &dyn Fn(&ColRef) -> bool) -> Option<Shape<'a>> {
    let Expr::Cmp { l, op, r } = x else { return None };
    let ((col, form), other, op) = match (converted(l, own), converted(r, own)) {
        (Some(w), None) => (w, r, op.clone()),
        (None, Some(w)) => (w, l, cond::flip(op)),
        _ => return None,
    };
    if !matches!(op.as_str(), "=" | ">=" | ">" | "<=" | "<") || !cond::term_cols(other).is_empty() {
        return None;
    }
    Some(Shape { col, form, op, lit: literal(other, form) })
}

/// The column of this table inside a date, year or text conversion.
fn converted<'a>(t: &'a Term, own: &dyn Fn(&ColRef) -> bool) -> Option<(&'a ColRef, Form)> {
    match t {
        Term::Cast { inner, ty } => match inner.as_ref() {
            Term::Col(c) if own(c) => match ty.as_str() {
                "date" => Some((c, Form::Date)),
                "text" | "char" | "varchar" | "character varying" => Some((c, Form::Text)),
                _ => None,
            },
            _ => None,
        },
        Term::Func { name, args } if args.len() == 1 => match (&args[0], name.as_str()) {
            (Term::Col(c), "date") if own(c) => Some((c, Form::Date)),
            (Term::Col(c), "year") if own(c) => Some((c, Form::Year)),
            _ => None,
        },
        _ => None,
    }
}

/// The value of a literal as the rewrite needs it: a date written YYYY-MM-DD, a year, or the text compared with.
fn literal(t: &Term, form: Form) -> Option<String> {
    let (s, ty) = match t {
        Term::Str(s) | Term::Num(s) => (s, None),
        Term::Cast { inner, ty } => match inner.as_ref() {
            Term::Str(s) | Term::Num(s) => (s, Some(ty.as_str())),
            _ => return None,
        },
        _ => return None,
    };
    match form {
        Form::Date if ty.is_none_or(|t| t == "date") => day(s).map(|_| s.clone()),
        Form::Year if ty.is_none() => year(s).map(|_| s.clone()),
        // Text compared with a number is no comparison of two texts.
        Form::Text if ty.is_none_or(cond::text_type) && !matches!(t, Term::Num(_)) && !matches!(t, Term::Cast { inner, .. } if matches!(inner.as_ref(), Term::Num(_))) => Some(s.clone()),
        _ => None,
    }
}

/// A date written exactly YYYY-MM-DD, in the years every engine stores.
fn day(s: &str) -> Option<NaiveDate> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    (d.format("%Y-%m-%d").to_string() == s && s.len() == 10 && !s.starts_with('0')).then_some(d)
}

/// A year with four digits whose next year still has four.
fn year(s: &str) -> Option<i32> {
    let y: i32 = s.parse().ok()?;
    ((1000..=9998).contains(&y) && y.to_string() == s).then_some(y)
}

/// The range a date or year comparison means, on `col`. The day after the last one stays in four-digit years.
fn range_sql(s: &Shape, col: &str, one_line: bool) -> Option<String> {
    let lit = s.lit.as_deref()?;
    let (lo, hi) = match s.form {
        Form::Date => {
            let d = day(lit)?;
            (d, d.succ_opt().filter(|n| n.format("%Y").to_string().len() == 4)?)
        }
        Form::Year => {
            let y = year(lit)?;
            (NaiveDate::from_ymd_opt(y, 1, 1)?, NaiveDate::from_ymd_opt(y + 1, 1, 1)?)
        }
        Form::Text => return None,
    };
    let q = |d: NaiveDate| format!("'{}'", d.format("%Y-%m-%d"));
    let and = if one_line { " AND " } else { "\nAND " };
    Some(match s.op.as_str() {
        "=" => format!("{col} >= {}{and}{col} < {}", q(lo), q(hi)),
        ">=" => format!("{col} >= {}", q(lo)),
        ">" => format!("{col} >= {}", q(hi)),
        "<=" => format!("{col} < {}", q(hi)),
        "<" => format!("{col} < {}", q(lo)),
        _ => return None,
    })
}

/// Dates, times and timestamps, the types a date or year of the column can be compared as a range on.
fn temporal(ty: &str, d: Dialect) -> bool {
    let t = ty.to_ascii_lowercase();
    if d == Dialect::Postgres {
        return t.starts_with("timestamp") || t == "date";
    }
    matches!(t.split(['(', ' ']).next().unwrap_or_default(), "datetime" | "timestamp" | "date")
}

fn dated(p: &Plan, i: usize, d: Dialect, t: &TableEv, s: &Shape) -> Option<Spot> {
    let ix = lead(t, &s.col.name, d)?;
    // A text column turned into a date would compare as text in a range, which is not the same rows.
    if !temporal(&t.col(&s.col.name)?.ty, d) {
        return None;
    }
    let how = if s.form == Form::Year { "takes its year" } else { "turns it into a date" };
    let body = vec![format!("{ix} covers {}, but the filter {how} first, so the index cannot be searched.", s.col.name), "Compare the column with a range instead and the index applies.".into()];
    let fix = range_sql(s, &col_text(s.col, d), false).map(|sql| Fix { kind: FixKind::Rewrite, label: "Rewritten filter".into(), sql, note: format!("Same rows, and {ix} can serve it."), trial: None });
    Some(Spot { index: ix, body, fix, no_fix: None, lines: base_lines(p, i, d, t) })
}

/// The smallest and largest value of an integer type, when `ty` is one.
fn int_range(ty: &str) -> Option<(i128, i128)> {
    let t = ty.to_ascii_lowercase();
    let bits = match t.split(['(', ' ']).next().unwrap_or_default() {
        "tinyint" => 8,
        "smallint" | "int2" => 16,
        "mediumint" => 24,
        "integer" | "int" | "int4" => 32,
        "bigint" | "int8" => 64,
        _ => return None,
    };
    Some(if t.contains("unsigned") { (0, (1i128 << bits) - 1) } else { (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1) })
}

/// Text types, whose cast to text is a relabel or close to one, which this does not judge.
fn textual(ty: &str) -> bool {
    let t = ty.to_ascii_lowercase();
    ["text", "varchar", "character varying", "char", "character", "citext", "tinytext", "mediumtext", "longtext", "enum", "set"].iter().any(|k| t.starts_with(k))
}

fn text_cast(p: &Plan, i: usize, d: Dialect, t: &TableEv, s: &Shape) -> Option<Spot> {
    let ix = lead(t, &s.col.name, d)?;
    let ty = t.col(&s.col.name)?.ty.clone();
    if textual(&ty) {
        return None;
    }
    let col = &s.col.name;
    let mut body = vec![format!("{ix} covers {col}, but the filter turns it into text first, so the index cannot be searched.")];
    // Only the integer's own way of writing itself is the same rows: '04242' and '4242 ' match no row as text.
    let n = s.lit.as_deref().filter(|_| s.op == "=").and_then(|l| l.parse::<i128>().ok().filter(|n| n.to_string() == l)).filter(|n| int_range(&ty).is_some_and(|(lo, hi)| (lo..=hi).contains(n)));
    let fix = n.map(|n| Fix { kind: FixKind::Rewrite, label: "Rewritten filter".into(), sql: format!("{} = {n}", col_text(s.col, d)), note: format!("Same rows, and {ix} can serve it."), trial: None });
    body.push(if fix.is_some() { format!("Compare {col} with a number and the index applies.") } else { format!("Compare {col} with a value of its own type and the index applies.") });
    Some(Spot { index: ix, body, fix, no_fix: None, lines: base_lines(p, i, d, t) })
}

// lower and upper.

/// The column and function of `lower(col) = ..`, `upper(col) IN (..)` and the like, compared with values.
fn fold_of<'a>(x: &'a Expr, own: &dyn Fn(&ColRef) -> bool) -> Option<(&'a ColRef, &'a str)> {
    let call = |t: &'a Term| -> Option<(&'a ColRef, &'a str)> {
        let Term::Func { name, args } = t else { return None };
        if !matches!(name.as_str(), "lower" | "upper") || args.len() != 1 {
            return None;
        }
        let c = match &args[0] {
            Term::Col(c) => c,
            Term::Cast { inner, ty } if cond::text_type(ty) => match inner.as_ref() {
                Term::Col(c) => c,
                _ => return None,
            },
            _ => return None,
        };
        own(c).then_some((c, name.as_str()))
    };
    let value = |t: &Term| cond::term_cols(t).is_empty() && !matches!(t, Term::Other(_) | Term::Func { .. });
    match x {
        Expr::Cmp { l, op, r } if matches!(op.as_str(), "=" | "<" | "<=" | ">" | ">=") => match (call(l), call(r)) {
            (Some(w), None) if value(r) => Some(w),
            (None, Some(w)) if value(l) => Some(w),
            _ => None,
        },
        Expr::In { l, list, not: false } if list.iter().all(value) => call(l),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn folded(p: &Plan, i: usize, d: Dialect, ev: &Evidence, t: &TableEv, x: &Expr, own: &dyn Fn(&ColRef) -> bool, pooler: bool) -> Option<Spot> {
    let (c, f) = fold_of(x, own)?;
    let ix = lead(t, &c.name, d)?;
    let col = &c.name;
    let mut body = vec![format!("{ix} covers {col}, but the filter wraps it in {f}(), so the index cannot be searched.")];
    let mut spot = Spot { index: ix, body: Vec::new(), fix: None, no_fix: None, lines: base_lines(p, i, d, t) };
    match d {
        Dialect::MariaDb => body.push(format!("MariaDB has no index on an expression. A generated column holding {f}({col}), with an index of its own, would serve a filter written on that column.")),
        Dialect::MySql if version_of(&ev.version) < (8, 0, 13) => body.push(format!("An index on {f}({col}) would serve it as it is written, which MySQL takes from 8.0.13 on.")),
        _ => {
            body.push(format!("An index on {f}({col}) serves the filter as it is written."));
            // Such an index may already exist, as an expression part the catalog read shows only as one.
            if let Some(e) = t.indexes.iter().flatten().find(|ix| ix.cols.first().is_some_and(String::is_empty)) {
                spot.no_fix = Some(format!("No index suggested: {} is on an expression, which may already be this one.", e.name));
            } else {
                let (lines, why) = gates(p, i, d, t);
                spot.lines = lines;
                match why {
                    Some(why) => spot.no_fix = Some(format!("No index suggested: {why}.")),
                    None => {
                        let name = name_for(d, t, &[f.to_string(), col.clone()], &[]);
                        let (sql, trial) = index::create(d, t, &name, &format!("({f}({}))", d.quote_ident(col)), How::Expr, pooler);
                        spot.fix = Some(Fix { kind: FixKind::Index, label: "Expression index".into(), sql, note: format!("An index on the expression exactly as the filter writes it. {NOTE}"), trial });
                    }
                }
            }
        }
    }
    spot.body = body;
    Some(spot)
}

// LIKE '%..'.

/// The longest run of characters in a LIKE pattern that are not wildcards.
fn literal_run(pat: &str) -> usize {
    let (mut best, mut run, mut esc) = (0, 0, false);
    for ch in pat.chars() {
        if !esc && ch == '\\' {
            esc = true;
            continue;
        }
        if !esc && (ch == '%' || ch == '_') {
            run = 0;
        } else {
            run += 1;
            best = best.max(run);
        }
        esc = false;
    }
    best
}

#[allow(clippy::too_many_arguments)]
fn wild(p: &Plan, i: usize, d: Dialect, ev: &Evidence, t: &TableEv, x: &Expr, own: &dyn Fn(&ColRef) -> bool, pooler: bool) -> Option<Spot> {
    let Expr::Like { t: term, pat, not: false, .. } = x else { return None };
    let c = match term {
        Term::Col(c) => c,
        Term::Cast { inner, ty } if cond::text_type(ty) => match inner.as_ref() {
            Term::Col(c) => c,
            _ => return None,
        },
        _ => return None,
    };
    let pat = match pat {
        Term::Str(s) => s,
        Term::Cast { inner, .. } => match inner.as_ref() {
            Term::Str(s) => s,
            _ => return None,
        },
        _ => return None,
    };
    if !own(c) || !pat.starts_with(['%', '_']) {
        return None;
    }
    let ix = lead(t, &c.name, d)?;
    let col = &c.name;
    let mut spot = Spot { index: ix.clone(), body: vec![format!("{ix} covers {col}, but the pattern starts with a wildcard, so the index cannot be searched.")], fix: None, no_fix: None, lines: base_lines(p, i, d, t) };
    if d != Dialect::Postgres {
        spot.body.push("An index is searched from the start of the text, so a pattern that starts with a wildcard reads every entry.".into());
        return Some(spot);
    }
    let ty = t.col(col).map(|c| c.ty.to_ascii_lowercase()).unwrap_or_default();
    if !ev.trgm {
        // Installing an extension is the database owner's call, so it is named and never written out.
        spot.body.push("A trigram index of the pg_trgm extension could serve it, and pg_trgm is not installed in this database.".into());
    } else if literal_run(pat) < TRGM_RUN {
        spot.body.push("The pattern has fewer than three characters in a row between its wildcards, too few for a trigram index to narrow.".into());
    } else if ty == "text" || ty.starts_with("character varying") || ty.starts_with("varchar") {
        spot.body.push("A trigram index of pg_trgm searches for any part of the text.".into());
        let (lines, why) = gates(p, i, d, t);
        spot.lines = lines;
        match why {
            Some(why) => spot.no_fix = Some(format!("No index suggested: {why}.")),
            None => {
                let name = name_for(d, t, &[col.clone(), "trgm".into()], &[]);
                let (sql, trial) = index::create(d, t, &name, &format!("{} gin_trgm_ops", d.quote_ident(col)), How::Gin, pooler);
                spot.fix = Some(Fix { kind: FixKind::Index, label: "Trigram index".into(), sql, note: format!("A GIN index of pg_trgm, which serves LIKE and ILIKE with a wildcard anywhere. It is larger than a B-tree and slower to update. {NOTE}"), trial });
            }
        }
    }
    Some(spot)
}

// OR across columns.

fn either(p: &Plan, i: usize, d: Dialect, t: &TableEv, x: &Expr, own: &dyn Fn(&ColRef) -> bool, pooler: bool) -> Option<Spot> {
    let Expr::Or(xs) = x else { return None };
    let ty = |c: &str| t.col(c).map(|c| c.ty.clone());
    let mut cols: Vec<String> = Vec::new();
    for b in xs {
        // Each side one condition an index on its column searches; anything else is past what is judged here.
        let s = cond::sargs(b, d, own, false, &ty);
        let [g] = s.sargs.as_slice() else { return None };
        if !s.blockers.is_empty() || s.other != 0 || !matches!(g.kind, SargKind::Eq | SargKind::In | SargKind::Range) {
            return None;
        }
        if !cols.iter().any(|c| index::same(c, &g.col, d)) {
            cols.push(g.col.clone());
        }
    }
    if cols.len() < 2 {
        return None;
    }
    let (served, missing): (Vec<&String>, Vec<&String>) = cols.iter().partition(|c| lead(t, c, d).is_some());
    let first = served.first()?;
    if missing.is_empty() {
        return None;
    }
    let ix = lead(t, first, d)?;
    let who = if d == Dialect::Postgres { "planner" } else { "server" };
    let names: Vec<String> = missing.iter().map(|c| c.to_string()).collect();
    let mut spot = Spot {
        index: ix.clone(),
        body: vec![format!("{ix} serves {first}, but {} {} no index, so the {who} cannot search every side of the OR and reads every row.", and_list(&names), if names.len() == 1 { "has" } else { "have" })],
        fix: None,
        no_fix: None,
        lines: base_lines(p, i, d, t),
    };
    if !names.iter().all(|c| t.col(c).is_some_and(|c| index::indexable(&c.ty, d))) {
        return Some(spot);
    }
    let (lines, why) = gates(p, i, d, t);
    spot.lines = lines;
    match why {
        Some(why) => spot.no_fix = Some(format!("No index suggested: {why}.")),
        None => {
            let mut made: Vec<String> = Vec::new();
            let mut sql = Vec::new();
            let mut trials = Vec::new();
            for c in &names {
                let name = name_for(d, t, std::slice::from_ref(c), &made);
                let (s, trial) = index::create(d, t, &name, &d.quote_ident(c), How::Plain, pooler);
                sql.push(s);
                trials.push(trial);
                made.push(name);
            }
            // A trial takes one index at a time, so only an OR missing a single index has one.
            let trial = if trials.len() == 1 { trials.pop().flatten() } else { None };
            spot.fix = Some(Fix { kind: FixKind::Index, label: "Candidate index".into(), sql: sql.join("\n"), note: format!("One index for each side of the OR, so the {who} can search them all and combine the rows. {NOTE}"), trial });
        }
    }
    Some(spot)
}

// Join buffers.

/// A candidate index on the column a join buffer matches on, when no index leads with it and the side before the join
/// gives few rows for the table's size: each of them then looks its matches up instead of the whole table going
/// through the buffer. MySQL flags the table itself and keeps the join's condition in its filter; MariaDB flags a step
/// above the table with the condition of its own.
fn join_buffers(p: &Plan, d: Dialect, ev: &Evidence, pooler: bool, out: &mut [Finding]) {
    if d == Dialect::Postgres {
        return;
    }
    for (i, n) in p.nodes.iter().enumerate() {
        if !n.join_buffer {
            continue;
        }
        let Some(k) = out.iter().position(|f| f.node == Some(i) && f.kind == Kind::JoinBuffer) else { continue };
        let (ti, e) = if n.alias.is_empty() { (n.kids.first().copied(), n.join_cond.expr.as_ref()) } else { (Some(i), n.filter.expr.as_ref()) };
        let (Some(ti), Some(e)) = (ti, e) else { continue };
        let tn = &p.nodes[ti];
        let Some(t) = ev.table(tn) else { continue };
        // Another finding on the table already carries an index; two for one scan would only confuse.
        if out.iter().any(|f| f.node == Some(ti) && f.fix.as_ref().is_some_and(|x| x.kind == FixKind::Index)) {
            continue;
        }
        let own = |c: &ColRef| c.qual.as_deref().is_none_or(|q| index::same(q, &tn.alias, d) || index::same(q, &t.table, d));
        let Some(col) = conjuncts(e).into_iter().find_map(|x| match x {
            Expr::Cmp { l: Term::Col(a), op, r: Term::Col(b) } if op == "=" => {
                if own(a) && b.qual.is_some() && !own(b) {
                    Some(a.name.clone())
                } else if own(b) && a.qual.is_some() && !own(a) {
                    Some(b.name.clone())
                } else {
                    None
                }
            }
            _ => None,
        }) else {
            continue;
        };
        let Some(c) = t.col(&col) else { continue };
        if !index::indexable(&c.ty, d) || t.indexes.as_ref().is_none_or(|ixs| ixs.iter().any(|ix| ix.btree && !ix.partial && ix.cols.first().is_some_and(|f| index::same(f, &col, d)))) {
            continue;
        }
        let Some(rows) = index::table_rows(p, ti, t, d).filter(|&r| r >= MIN_ROWS) else { continue };
        if index::stale(t).is_some() {
            continue;
        }
        let Some(outer) = feeding(p, i) else { continue };
        if outer > MAX_KEPT * rows {
            continue;
        }
        let mut lines = base_lines(p, ti, d, t);
        lines.push(if p.analyzed { format!("The tables before it gave {} rows to look up.", count(outer)) } else { format!("The tables before it give at most about {} rows to look up, by the server's estimate.", count(outer)) });
        if let Some(nd) = index::distinct(t, &col, rows) {
            lines.push(format!("Distinct values: {col} {}.", count(nd)));
        }
        let name = name_for(d, t, std::slice::from_ref(&col), &[]);
        let (sql, trial) = index::create(d, t, &name, &d.quote_ident(&col), How::Plain, pooler);
        out[k].fix = Some(Fix { kind: FixKind::Index, label: "Candidate index".into(), sql, note: format!("An index on the join column, so each row of the tables before it looks its matches up. {NOTE}"), trial });
        out[k].evidence = lines;
        // The scan's own finding says its filter's share is a guess; the join's candidate does not rest on that share.
        for f in out.iter_mut().filter(|f| f.node == Some(ti) && f.kind == Kind::FullScan && f.fix.is_none()) {
            f.no_fix = None;
        }
    }
}

/// The rows the steps before `i` in its nested loop feed it, at most: what each read, multiplied, or what each kept
/// once ANALYZE counted it.
fn feeding(p: &Plan, i: usize) -> Option<f64> {
    let parent = &p.nodes[p.nodes[i].parent?];
    let before = parent.kids.iter().take_while(|&&k| k != i).copied().collect::<Vec<_>>();
    if before.is_empty() {
        return None;
    }
    before.iter().try_fold(1.0, |acc, &k| {
        let s = &p.nodes[k];
        if s.alias.is_empty() {
            return None;
        }
        let rows = if p.analyzed && !s.never_ran { s.kept_rows().map(|r| r * s.loops.unwrap_or(1.0)) } else { s.rows };
        Some(acc * rows?.max(1.0))
    })
}

// The statement with the rewrite in place.

/// `stmt` with the condition that `fix` rewrites replaced by it, for the draft. The plan prints its own form of the
/// condition, so the statement is read token by token for the same column, conversion, operator and value: it has to
/// appear exactly once, between words where a condition starts and ends (WHERE, AND, OR, ON, parentheses), and the
/// statement must have no BETWEEN, whose AND reads like a condition's. Otherwise None, and the draft is the condition.
pub fn statement(stmt: &str, d: Dialect, n: &PlanNode, fix: &Fix) -> Option<String> {
    if fix.kind != FixKind::Rewrite {
        return None;
    }
    let own = |c: &ColRef| c.qual.as_deref().is_none_or(|q| index::same(q, &n.alias, d) || index::same(q, &n.rel, d));
    let s = conjuncts(n.filter.expr.as_ref()?).into_iter().find_map(|x| shape(x, &own).filter(|s| plan_sql(s, d).as_deref() == Some(fix.sql.as_str())))?;
    let toks = significant(stmt, d);
    let word = |j: usize, w: &str| toks.get(j).is_some_and(|(t, r)| *t == Tok::Word && stmt[r.clone()].eq_ignore_ascii_case(w));
    if (0..toks.len()).any(|j| word(j, "between")) {
        return None;
    }
    let m = Matcher { stmt, toks: &toks, d, s: &s, n };
    let hits: Vec<(usize, usize, Range<usize>)> = (0..toks.len()).filter_map(|j| m.at(j).map(|(end, col)| (j, end, col))).collect();
    let [(j, end, col)] = hits.as_slice() else { return None };
    let punct = |k: usize, p: &str| toks.get(k).is_some_and(|(t, r)| *t == Tok::Punct && &stmt[r.clone()] == p);
    let starts = *j > 0 && (["where", "and", "or", "on", "having"].iter().any(|w| word(j - 1, w)) || punct(j - 1, "("));
    let ends = *end >= toks.len() || punct(*end, ")") || punct(*end, ";") || ["and", "or", "group", "order", "limit", "having", "window", "union", "except", "intersect", "offset", "fetch", "for", "returning"].iter().any(|w| word(*end, w));
    if !starts || !ends {
        return None;
    }
    let text = match s.form {
        Form::Text => format!("{} = {}", &stmt[col.clone()], s.lit.as_deref()?),
        _ => range_sql(&s, &stmt[col.clone()], true)?,
    };
    // AND binds tighter than OR, so the range needs no parentheses there; they only make it read as one condition.
    let beside_or = word(j - 1, "or") || word(*end, "or");
    let text = if beside_or && text.contains(" AND ") { format!("({text})") } else { text };
    Some(format!("{}{text}{}", &stmt[..toks[*j].1.start], &stmt[toks[*end - 1].1.end..]))
}

/// The rewrite of `s` as the plan's column names write it, which is what the finding's fix holds.
fn plan_sql(s: &Shape, d: Dialect) -> Option<String> {
    match s.form {
        Form::Text => Some(format!("{} = {}", col_text(s.col, d), s.lit.as_deref()?)),
        _ => range_sql(s, &col_text(s.col, d), false),
    }
}

/// Tokens without spaces and comments, with two-character operators joined.
fn significant(text: &str, d: Dialect) -> Vec<(Tok, Range<usize>)> {
    let mut out: Vec<(Tok, Range<usize>)> = Vec::new();
    for (t, r) in lex::tokens(text, d) {
        if matches!(t, Tok::Space | Tok::Comment | Tok::ExecOpen | Tok::ExecClose) {
            continue;
        }
        if let (Tok::Punct, Some((Tok::Punct, last))) = (t, out.last_mut()) {
            if last.end == r.start && matches!(format!("{}{}", &text[last.clone()], &text[r.clone()]).as_str(), ">=" | "<=" | "<>" | "!=" | "::") {
                last.end = r.end;
                continue;
            }
        }
        out.push((t, r));
    }
    out
}

/// Finds the condition of one shape in a statement's tokens.
struct Matcher<'a> {
    stmt: &'a str,
    toks: &'a [(Tok, Range<usize>)],
    d: Dialect,
    s: &'a Shape<'a>,
    n: &'a PlanNode,
}

impl Matcher<'_> {
    fn text(&self, j: usize) -> Option<&str> {
        self.toks.get(j).map(|(_, r)| &self.stmt[r.clone()])
    }

    fn is(&self, j: usize, s: &str) -> bool {
        self.text(j).is_some_and(|t| t.eq_ignore_ascii_case(s))
    }

    /// A name token as the server reads it.
    fn ident(&self, j: usize) -> Option<String> {
        let (t, r) = self.toks.get(j)?;
        let s = &self.stmt[r.clone()];
        match t {
            Tok::Word if s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') => Some(if self.d == Dialect::Postgres { s.to_lowercase() } else { s.to_string() }),
            Tok::Quoted => Some(lex::unquote_ident(s)),
            Tok::Str if self.d == Dialect::Postgres && s.starts_with('"') => Some(lex::unquote_ident(s)),
            _ => None,
        }
    }

    /// The condition starting at token `j`: the token after it and the column's bytes.
    fn at(&self, j: usize) -> Option<(usize, Range<usize>)> {
        if let Some((k, col)) = self.conversion(j) {
            if self.is(k, &self.s.op) {
                let end = self.value(k + 1)?;
                return Some((end, col));
            }
            return None;
        }
        let k = self.value(j)?;
        if !self.is(k, &cond::flip(&self.s.op)) {
            return None;
        }
        let (end, col) = self.conversion(k + 1)?;
        Some((end, col))
    }

    /// `name`, `alias.name` or `schema.table.name` for the shape's column; the token after it and its bytes.
    fn column(&self, j: usize) -> Option<(usize, Range<usize>)> {
        // The middle of a qualified name is no name of its own.
        if j > 0 && self.is(j - 1, ".") {
            return None;
        }
        let mut parts = vec![self.ident(j)?];
        let mut k = j + 1;
        while self.is(k, ".") && parts.len() < 3 {
            parts.push(self.ident(k + 1)?);
            k += 2;
        }
        let same = |a: &str, b: &str| index::same(a, b, self.d);
        let name = parts.pop()?;
        let qual_ok = parts.last().is_none_or(|q| same(q, &self.n.alias) || same(q, &self.n.rel));
        (same(&name, &self.s.col.name) && qual_ok).then(|| (k, self.toks[j].1.start..self.toks[k - 1].1.end))
    }

    /// The shape's conversion of its column, in any of the ways it is written.
    fn conversion(&self, j: usize) -> Option<(usize, Range<usize>)> {
        let types: &[&str] = match self.s.form {
            Form::Date => &["date"],
            Form::Text => &["text", "varchar", "char"],
            Form::Year => &[],
        };
        let func = match self.s.form {
            Form::Date => Some("date"),
            Form::Year => Some("year"),
            Form::Text => None,
        };
        // date(col), year(col)
        if func.is_some_and(|f| self.is(j, f)) && self.is(j + 1, "(") {
            let (k, col) = self.column(j + 2)?;
            return self.is(k, ")").then_some((k + 1, col));
        }
        // cast(col as date)
        if self.is(j, "cast") && self.is(j + 1, "(") && !types.is_empty() {
            let (k, col) = self.column(j + 2)?;
            if self.is(k, "as") && types.iter().any(|t| self.is(k + 1, t)) && self.is(k + 2, ")") {
                return Some((k + 3, col));
            }
            return None;
        }
        // col::date
        let (k, col) = self.column(j)?;
        (self.is(k, "::") && types.iter().any(|t| self.is(k + 1, t))).then_some((k + 2, col))
    }

    /// The shape's value, as `'2026-03-01'`, `'2026-03-01'::date`, `DATE '2026-03-01'` or `2026`; the token after it.
    fn value(&self, j: usize) -> Option<usize> {
        let lit = self.s.lit.as_deref()?;
        let str_at = |k: usize| match self.toks.get(k) {
            Some((Tok::Str, r)) if self.stmt[r.clone()].starts_with('\'') => Some(cond::unquote_str(&self.stmt[r.clone()])),
            _ => None,
        };
        let (k, v) = match self.s.form {
            Form::Date if self.is(j, "date") => (j + 2, str_at(j + 1)?),
            Form::Year => match self.toks.get(j) {
                Some((Tok::Word, r)) => (j + 1, self.stmt[r.clone()].to_string()),
                _ => (j + 1, str_at(j)?),
            },
            _ => (j + 1, str_at(j)?),
        };
        if v != lit {
            return None;
        }
        // A cast the user wrote on the literal is part of it, when it is to the type the comparison already has.
        let types: &[&str] = if self.s.form == Form::Date { &["date"] } else { &["text", "varchar"] };
        if self.is(k, "::") {
            return (self.s.form != Form::Year && types.iter().any(|t| self.is(k + 1, t))).then_some(k + 2);
        }
        Some(k)
    }
}

#[cfg(test)]
mod tests {
    use super::super::evidence::{ColEv, IndexEv};
    use super::super::parse;
    use super::*;

    #[test]
    fn a_column_named_like_a_keyword_is_quoted_in_a_rewrite() {
        assert_eq!(ident_text("created_at", Dialect::Postgres), "created_at");
        assert_eq!(ident_text("order", Dialect::Postgres), "\"order\"");
        assert_eq!(ident_text("user", Dialect::MySql), "`user`");
        assert_eq!(col_text(&ColRef { qual: Some("o".into()), name: "key".into() }, Dialect::MySql), "o.`key`");
    }

    fn fixture(name: &str) -> &'static str {
        match name {
            "pg18-datewrap" => include_str!("fixtures/pg18-datewrap.json"),
            "pg18-datefn" => include_str!("fixtures/pg18-datefn.json"),
            "pg18-lower" => include_str!("fixtures/pg18-lower.json"),
            "pg18-like" => include_str!("fixtures/pg18-like.json"),
            "pg18-cast" => include_str!("fixtures/pg18-cast.json"),
            "pg18-or" => include_str!("fixtures/pg18-or.json"),
            "mysql-datewrap" => include_str!("fixtures/mysql-datewrap.json"),
            "mysql-datewrapall" => include_str!("fixtures/mysql-datewrapall.json"),
            "mysql-datewrapall-analyze" => include_str!("fixtures/mysql-datewrapall-analyze.txt"),
            "mysql-join" => include_str!("fixtures/mysql-join.json"),
            "mysql-yearall" => include_str!("fixtures/mysql-yearall.json"),
            "mysql-lowerall" => include_str!("fixtures/mysql-lowerall.json"),
            "mysql-lowerall-analyze" => include_str!("fixtures/mysql-lowerall-analyze.txt"),
            "mysql-likeall" => include_str!("fixtures/mysql-likeall.json"),
            "mysql-orall" => include_str!("fixtures/mysql-orall.json"),
            "mysql-orall-analyze" => include_str!("fixtures/mysql-orall-analyze.txt"),
            "mysql-castint" => include_str!("fixtures/mysql-castint.json"),
            "mysql-hashjoin" => include_str!("fixtures/mysql-hashjoin.json"),
            "mysql-bufjoin" => include_str!("fixtures/mysql-bufjoin.json"),
            "mariadb-yearall" => include_str!("fixtures/mariadb-yearall.json"),
            "mariadb-lowerall-analyze" => include_str!("fixtures/mariadb-lowerall-analyze.json"),
            "mariadb-likeall" => include_str!("fixtures/mariadb-likeall.json"),
            "mariadb-orall-analyze" => include_str!("fixtures/mariadb-orall-analyze.json"),
            "mariadb-castint-analyze" => include_str!("fixtures/mariadb-castint-analyze.json"),
            "mariadb-bufjoin" => include_str!("fixtures/mariadb-bufjoin.json"),
            "mysql-datewrap-analyze" => include_str!("fixtures/mysql-datewrap-analyze.txt"),
            "mysql-lower-analyze" => include_str!("fixtures/mysql-lower-analyze.txt"),
            "mysql-upperall-analyze" => include_str!("fixtures/mysql-upperall-analyze.txt"),
            "mariadb-upperall-analyze" => include_str!("fixtures/mariadb-upperall-analyze.json"),
            _ => panic!("no fixture {name}"),
        }
    }

    fn ix(name: &str, cols: &[&str]) -> IndexEv {
        IndexEv { name: name.into(), cols: cols.iter().map(|c| c.to_string()).collect(), btree: true, ..Default::default() }
    }

    fn col(name: &str, ty: &str) -> ColEv {
        ColEv { name: name.into(), ty: ty.into(), null_frac: Some(0.0), ..Default::default() }
    }

    /// The PostgreSQL captures' bookings, or MySQL's and MariaDB's orders, as the catalog reads them, under the names
    /// the plan reads the table by, with `indexes`.
    fn table(d: Dialect, names: &[(&str, &str)], indexes: Vec<IndexEv>) -> TableEv {
        let names = names.iter().map(|(r, a)| (r.to_string(), a.to_string())).collect();
        if d == Dialect::Postgres {
            let cols = vec![col("id", "bigint"), col("created_at", "timestamp with time zone"), col("status", "booking_status"), col("email", "character varying(120)"), col("ref", "text"), col("note", "text")];
            return TableEv { names, schema: "public".into(), table: "bookings".into(), rows: Some(400_000.0), bytes: Some(5e7), analyzed_ago_s: Some(3600.0), mods_since: Some(10.0), indexes: Some(indexes), cols, ..Default::default() };
        }
        let cols = vec![col("id", "int"), col("customer_id", "int"), col("created_at", "datetime"), col("status", "varchar(16)"), col("total", "decimal(10,2)"), col("email", "varchar(120)"), col("ref", "varchar(20)")];
        TableEv { names, schema: "shop".into(), table: "orders".into(), rows: Some(199_143.0), bytes: Some(5e7), indexes: Some(indexes), cols, ..Default::default() }
    }

    /// The captures' own indexes: the primary key, created_at and email.
    fn captured(d: Dialect) -> Vec<IndexEv> {
        if d == Dialect::Postgres {
            vec![ix("bookings_pkey", &["id"]), ix("bookings_created_at_idx", &["created_at"]), ix("bookings_email_idx", &["email"])]
        } else {
            vec![ix("PRIMARY", &["id"]), ix("idx_orders_created_at", &["created_at"]), ix("idx_orders_email", &["email"])]
        }
    }

    fn ev(d: Dialect, t: TableEv) -> Evidence {
        let version = if d == Dialect::Postgres { "18.6" } else if d == Dialect::MariaDb { "11.4.3-MariaDB" } else { "8.4.3" };
        Evidence { tables: vec![t], version: version.into(), ..Default::default() }
    }

    /// The plan `name` with the findings worked out on `ev`.
    fn found(d: Dialect, name: &str, ev: &Evidence) -> Plan {
        let mut p = parse(d, fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        p.refind(d, Some(ev), false);
        p
    }

    /// The finding about an index the filter defeats, if there is one.
    fn unused(p: &Plan) -> Option<&Finding> {
        p.findings.iter().find(|f| f.kind == Kind::UnusedIndex)
    }

    fn rewrite(p: &Plan) -> Option<String> {
        unused(p).and_then(|f| f.fix.as_ref()).filter(|x| x.kind == FixKind::Rewrite).map(|x| x.sql.clone())
    }

    /// The statement a HypoPG trial of the plan's candidate index takes.
    fn trial_of(p: &Plan) -> Option<String> {
        p.findings.iter().find_map(|f| f.fix.as_ref().filter(|x| x.kind == FixKind::Index)).and_then(|x| x.trial.clone())
    }

    fn index_fix(p: &Plan) -> Option<String> {
        p.findings.iter().find_map(|f| f.fix.as_ref().filter(|x| x.kind == FixKind::Index).map(|x| x.sql.clone()))
    }

    fn pg(name: &str, indexes: Vec<IndexEv>) -> Plan {
        found(Dialect::Postgres, name, &ev(Dialect::Postgres, table(Dialect::Postgres, &[("bookings", "bookings")], indexes)))
    }

    fn my(d: Dialect, name: &str, alias: &str, indexes: Vec<IndexEv>) -> Plan {
        found(d, name, &ev(d, table(d, &[("", alias)], indexes)))
    }

    #[test]
    fn a_date_wrap_becomes_a_range_where_an_index_leads_with_the_column() {
        for name in ["pg18-datewrap", "pg18-datefn"] {
            let p = pg(name, captured(Dialect::Postgres));
            let f = unused(&p).unwrap_or_else(|| panic!("{name}: {:?}", p.findings));
            assert_eq!(f.title, "bookings has an index the planner did not use");
            assert!(f.body[0].contains("bookings_created_at_idx covers created_at, but the filter turns it into a date first"), "{:?}", f.body);
            let fix = f.fix.as_ref().unwrap();
            assert_eq!((fix.kind, fix.sql.as_str(), fix.note.as_str()), (FixKind::Rewrite, "created_at >= '2026-03-01'\nAND created_at < '2026-03-02'", "Same rows, and bookings_created_at_idx can serve it."));
            // It goes first, before the full scan it explains.
            assert_eq!(p.findings[0].kind, Kind::UnusedIndex, "{name}");
            // No index on created_at: nothing to say about one.
            let p = pg(name, vec![ix("bookings_pkey", &["id"])]);
            assert!(unused(&p).is_none(), "{name}: {:?}", unused(&p));
        }
        // MySQL's plan, a full scan or a full scan of the index itself, and their EXPLAIN ANALYZE trees.
        for name in ["mysql-datewrapall", "mysql-datewrap", "mysql-datewrapall-analyze", "mysql-datewrap-analyze"] {
            let p = my(Dialect::MySql, name, "orders", captured(Dialect::MySql));
            assert_eq!(rewrite(&p).as_deref(), Some("orders.created_at >= '2026-03-01'\nAND orders.created_at < '2026-03-02'"), "{name}: {:?}", p.findings);
            assert!(rewrite(&my(Dialect::MySql, name, "orders", vec![ix("PRIMARY", &["id"])])).is_none(), "{name}");
        }
        // The design's plan: a guessed share keeps an index from being suggested, not the rewrite.
        let p = my(Dialect::MySql, "mysql-join", "o", captured(Dialect::MySql));
        assert_eq!(rewrite(&p).as_deref(), Some("o.created_at >= '2026-11-01'\nAND o.created_at < '2026-11-02'"));
        assert!(p.findings.iter().any(|f| f.title == "Full scan of o"), "{:?}", p.findings);
    }

    #[test]
    fn year_becomes_a_range_on_mysql_and_mariadb_needs_nothing() {
        let p = my(Dialect::MySql, "mysql-yearall", "orders", captured(Dialect::MySql));
        assert_eq!(rewrite(&p).as_deref(), Some("orders.created_at >= '2026-01-01'\nAND orders.created_at < '2027-01-01'"));
        assert!(unused(&p).unwrap().body[0].contains("takes its year first"));
        assert!(rewrite(&my(Dialect::MySql, "mysql-yearall", "orders", vec![ix("PRIMARY", &["id"])])).is_none());
        // MariaDB turns YEAR() into a range itself, so its plan has no wrap and keeps the generic possible keys sentence.
        let p = my(Dialect::MariaDb, "mariadb-yearall", "orders", captured(Dialect::MariaDb));
        assert!(rewrite(&p).is_none());
        if let Some(f) = unused(&p) {
            assert!(f.body[0].starts_with("Possible keys:"), "{:?}", f.body);
        }
    }

    fn shape_of(filter: &str, d: Dialect) -> Option<String> {
        let e = cond::parse(filter, d).unwrap();
        let s = shape(&e, &|_| true)?;
        range_sql(&s, "c", true)
    }

    #[test]
    fn ranges_cross_month_and_year_ends_and_follow_the_operator() {
        let pg = |f: &str| shape_of(f, Dialect::Postgres);
        assert_eq!(pg("((c)::date = '2026-01-31'::date)").as_deref(), Some("c >= '2026-01-31' AND c < '2026-02-01'"));
        assert_eq!(pg("((c)::date = '2026-12-31'::date)").as_deref(), Some("c >= '2026-12-31' AND c < '2027-01-01'"));
        assert_eq!(pg("((c)::date = '2028-02-28'::date)").as_deref(), Some("c >= '2028-02-28' AND c < '2028-02-29'"));
        assert_eq!(pg("((c)::date = '2027-02-28'::date)").as_deref(), Some("c >= '2027-02-28' AND c < '2027-03-01'"));
        assert_eq!(pg("((c)::date >= '2026-03-01'::date)").as_deref(), Some("c >= '2026-03-01'"));
        assert_eq!(pg("((c)::date > '2026-03-31'::date)").as_deref(), Some("c >= '2026-04-01'"));
        assert_eq!(pg("((c)::date <= '2026-03-31'::date)").as_deref(), Some("c < '2026-04-01'"));
        assert_eq!(pg("((c)::date < '2026-03-01'::date)").as_deref(), Some("c < '2026-03-01'"));
        // A value on the left reads the other way round.
        assert_eq!(pg("('2026-03-01'::date <= (c)::date)").as_deref(), Some("c >= '2026-03-01'"));
        let my = |f: &str| shape_of(f, Dialect::MySql);
        assert_eq!(my("(year(`s`.`t`.`c`) = 2026)").as_deref(), Some("c >= '2026-01-01' AND c < '2027-01-01'"));
        assert_eq!(my("(year(`s`.`t`.`c`) > 2026)").as_deref(), Some("c >= '2027-01-01'"));
        assert_eq!(my("(year(`s`.`t`.`c`) <= 2026)").as_deref(), Some("c < '2027-01-01'"));
        // What is not a plain day or year, or has no day after it in four digits, is left alone.
        for f in ["((c)::date = '2026-02-30'::date)", "((c)::date = '2026-3-1'::date)", "((c)::date = '9999-12-31'::date)", "((c)::date <> '2026-03-01'::date)", "((c)::date = $1)", "((c)::date = CURRENT_DATE)"] {
            assert_eq!(pg(f), None, "{f}");
        }
        assert_eq!(my("(year(`s`.`t`.`c`) = 9999)"), None);
        assert_eq!(my("(year(`s`.`t`.`c`) = 02026)"), None);
    }

    #[test]
    fn a_date_wrap_on_a_column_that_is_not_a_date_gives_nothing() {
        let mut t = table(Dialect::Postgres, &[("bookings", "bookings")], captured(Dialect::Postgres));
        t.cols[1].ty = "text".into();
        let p = found(Dialect::Postgres, "pg18-datewrap", &ev(Dialect::Postgres, t));
        assert!(rewrite(&p).is_none());
    }

    #[test]
    fn a_number_compared_as_text_compares_as_a_number_again() {
        let p = pg("pg18-cast", captured(Dialect::Postgres));
        let f = unused(&p).unwrap();
        assert_eq!(f.fix.as_ref().map(|x| x.sql.as_str()), Some("id = 4242"));
        assert!(f.body[0].contains("bookings_pkey covers id, but the filter turns it into text first"), "{:?}", f.body);
        // MySQL and MariaDB print the cast with its character set.
        assert_eq!(rewrite(&my(Dialect::MySql, "mysql-castint", "orders", captured(Dialect::MySql))).as_deref(), Some("orders.id = 4242"));
        assert_eq!(rewrite(&my(Dialect::MariaDb, "mariadb-castint-analyze", "orders", captured(Dialect::MariaDb))).as_deref(), Some("orders.id = 4242"));
        // Without an index on the column, nothing.
        assert!(unused(&pg("pg18-cast", vec![ix("bookings_email_idx", &["email"])])).is_none());
        assert!(unused(&my(Dialect::MySql, "mysql-castint", "orders", vec![ix("idx_orders_email", &["email"])])).is_none());
        assert!(unused(&my(Dialect::MariaDb, "mariadb-castint-analyze", "orders", vec![ix("idx_orders_email", &["email"])])).is_none());
        // A literal the integer never prints as matches no row as text, so it is no rewrite, only the explanation.
        for lit in ["04242", "4242 ", "+4242", "42.0"] {
            let json = fixture("pg18-cast").replace("'4242'", &format!("'{lit}'"));
            let mut p = parse(Dialect::Postgres, &json).unwrap();
            p.refind(Dialect::Postgres, Some(&ev(Dialect::Postgres, table(Dialect::Postgres, &[("bookings", "bookings")], captured(Dialect::Postgres)))), false);
            let f = unused(&p).unwrap_or_else(|| panic!("{lit}"));
            assert!(f.fix.is_none() && f.body[1].contains("value of its own type"), "{lit}: {f:?}");
        }
        // Past the column type's range, the same.
        let mut t = table(Dialect::Postgres, &[("bookings", "bookings")], captured(Dialect::Postgres));
        t.cols[0].ty = "smallint".into();
        let json = fixture("pg18-cast").replace("'4242'", "'99999'");
        let mut p = parse(Dialect::Postgres, &json).unwrap();
        p.refind(Dialect::Postgres, Some(&ev(Dialect::Postgres, t)), false);
        assert!(unused(&p).unwrap().fix.is_none());
    }

    #[test]
    fn a_varchar_relabel_is_never_a_cast() {
        // `(email)::text = '..'` on a varchar column is the column itself: the index on email serves it.
        let filter = "((email)::text = 'user42@example.com'::text)";
        let t = table(Dialect::Postgres, &[("bookings", "bookings")], captured(Dialect::Postgres));
        let mut p = parse(Dialect::Postgres, &fixture("pg18-cast").replace("((id)::text = '4242'::text)", filter)).unwrap();
        p.refind(Dialect::Postgres, Some(&ev(Dialect::Postgres, t.clone())), false);
        assert!(p.findings.iter().all(|f| !f.body.iter().any(|b| b.contains("into text")) && f.fix.as_ref().is_none_or(|x| x.kind != FixKind::Rewrite)), "{:?}", p.findings);
        let e = cond::parse(filter, Dialect::Postgres).unwrap();
        let s = shape(&e, &|_| true).unwrap();
        assert!(text_cast(&p, 1, Dialect::Postgres, &t, &s).is_none());
    }

    #[test]
    fn lower_gets_an_expression_index_where_the_server_takes_one() {
        let p = pg("pg18-lower", captured(Dialect::Postgres));
        let f = unused(&p).unwrap();
        assert!(f.body[0].contains("wraps it in lower()"), "{:?}", f.body);
        assert_eq!(index_fix(&p).as_deref(), Some(r#"CREATE INDEX CONCURRENTLY "bookings_lower_email_idx" ON "public"."bookings" ((lower("email")));"#));
        assert!(pg("pg18-lower", vec![ix("bookings_pkey", &["id"])]).findings.iter().all(|f| f.kind != Kind::UnusedIndex));
        // An expression index already there may be this one.
        let mut ixs = captured(Dialect::Postgres);
        ixs.push(ix("bookings_lemail", &[""]));
        let p = pg("pg18-lower", ixs);
        assert_eq!(index_fix(&p), None);
        assert!(unused(&p).unwrap().no_fix.as_deref().unwrap().contains("bookings_lemail is on an expression"));
        // MySQL measured by ANALYZE: 5 rows of 200,000.
        let p = my(Dialect::MySql, "mysql-lowerall-analyze", "orders", captured(Dialect::MySql));
        assert_eq!(index_fix(&p).as_deref(), Some("-- Blocks writes to orders while it builds: MySQL adds an index on an expression under a shared lock.\nALTER TABLE `shop`.`orders` ADD INDEX `orders_lower_email_idx` ((lower(`email`))), ALGORITHM=INPLACE, LOCK=SHARED;"));
        // The same through a full scan of the index on email, as EXPLAIN ANALYZE's tree prints it.
        assert!(index_fix(&my(Dialect::MySql, "mysql-lower-analyze", "orders", captured(Dialect::MySql))).is_some_and(|s| s.contains("((lower(`email`)))")));
        // upper is the same.
        assert!(index_fix(&my(Dialect::MySql, "mysql-upperall-analyze", "orders", captured(Dialect::MySql))).is_some_and(|s| s.contains("`orders_upper_email_idx` ((upper(`email`)))")));
        let f = my(Dialect::MariaDb, "mariadb-upperall-analyze", "orders", captured(Dialect::MariaDb));
        assert!(unused(&f).is_some_and(|f| f.fix.is_none() && f.body[0].contains("wraps it in upper()")), "{:?}", f.findings);
        // No index on email, nothing.
        for (d, name) in [(Dialect::MySql, "mysql-lowerall-analyze"), (Dialect::MariaDb, "mariadb-lowerall-analyze")] {
            assert!(unused(&my(d, name, "orders", vec![ix("PRIMARY", &["id"])])).is_none(), "{name}");
        }
        // Its estimate alone is a guess: the finding, no index.
        let p = my(Dialect::MySql, "mysql-lowerall", "orders", captured(Dialect::MySql));
        assert_eq!(index_fix(&p), None);
        assert!(unused(&p).unwrap().no_fix.as_deref().unwrap().contains("Run with ANALYZE"));
        // Before 8.0.13 MySQL has no index on an expression.
        let mut e = ev(Dialect::MySql, table(Dialect::MySql, &[("", "orders")], captured(Dialect::MySql)));
        e.version = "8.0.12".into();
        let p = found(Dialect::MySql, "mysql-lowerall-analyze", &e);
        assert_eq!(index_fix(&p), None);
        assert!(unused(&p).unwrap().body[1].contains("8.0.13"));
        // MariaDB has none at all: the explanation only.
        let p = my(Dialect::MariaDb, "mariadb-lowerall-analyze", "orders", captured(Dialect::MariaDb));
        let f = unused(&p).unwrap();
        assert!(f.fix.is_none() && f.no_fix.is_none() && f.body[1].starts_with("MariaDB has no index on an expression"), "{f:?}");
        assert_eq!(index_fix(&p), None);
    }

    #[test]
    fn a_leading_wildcard_gets_a_trigram_index_only_where_pg_trgm_is_installed() {
        let t = || table(Dialect::Postgres, &[("bookings", "bookings")], captured(Dialect::Postgres));
        let p = found(Dialect::Postgres, "pg18-like", &ev(Dialect::Postgres, t()));
        let f = unused(&p).unwrap();
        assert!(f.fix.is_none() && f.body[1].contains("pg_trgm is not installed"), "{f:?}");
        let mut e = ev(Dialect::Postgres, t());
        e.trgm = true;
        let p = found(Dialect::Postgres, "pg18-like", &e);
        assert_eq!(index_fix(&p).as_deref(), Some(r#"CREATE INDEX CONCURRENTLY "bookings_email_trgm_idx" ON "public"."bookings" USING gin ("email" gin_trgm_ops);"#));
        assert_eq!(trial_of(&p), None, "HypoPG is offered a GIN index");
        let mut bare = ev(Dialect::Postgres, table(Dialect::Postgres, &[("bookings", "bookings")], vec![ix("bookings_pkey", &["id"])]));
        bare.trgm = true;
        assert!(unused(&found(Dialect::Postgres, "pg18-like", &bare)).is_none());
        // Two letters between wildcards give a trigram index nothing to search for.
        let mut p = parse(Dialect::Postgres, &fixture("pg18-like").replace("%42@example.com", "%42%")).unwrap();
        p.refind(Dialect::Postgres, Some(&e), false);
        assert_eq!(index_fix(&p), None);
        // MySQL and MariaDB: why, and nothing to create.
        for (d, name) in [(Dialect::MySql, "mysql-likeall"), (Dialect::MariaDb, "mariadb-likeall")] {
            let p = my(d, name, "orders", captured(d));
            let f = unused(&p).unwrap_or_else(|| panic!("{name}"));
            assert!(f.fix.is_none() && f.body[1].contains("from the start of the text"), "{name}: {f:?}");
            assert!(unused(&my(d, name, "orders", vec![])).is_none(), "{name}");
        }
        assert_eq!(literal_run("%ab\\%c%"), 4);
    }

    #[test]
    fn an_or_across_columns_gets_an_index_for_the_side_without_one() {
        let p = pg("pg18-or", captured(Dialect::Postgres));
        let f = unused(&p).unwrap();
        assert!(f.body[0].contains("bookings_email_idx serves email, but ref has no index"), "{:?}", f.body);
        assert_eq!(index_fix(&p).as_deref(), Some(r#"CREATE INDEX CONCURRENTLY "bookings_ref_idx" ON "public"."bookings" ("ref");"#));
        assert_eq!(trial_of(&p).as_deref(), Some(r#"CREATE INDEX "bookings_ref_idx" ON "public"."bookings" ("ref")"#));
        // Without an index on either side, nothing; with both, no index to add.
        assert!(unused(&pg("pg18-or", vec![ix("bookings_pkey", &["id"])])).is_none());
        let mut both = captured(Dialect::Postgres);
        both.push(ix("bookings_ref_idx", &["ref"]));
        assert_eq!(index_fix(&pg("pg18-or", both)), None);
        // MySQL's estimate names the OR and asks for a measure; ANALYZE gives the index.
        let p = my(Dialect::MySql, "mysql-orall", "orders", captured(Dialect::MySql));
        let f = unused(&p).unwrap();
        assert!(f.body[0].contains("idx_orders_email serves email, but ref has no index") && f.no_fix.as_deref().unwrap().contains("Run with ANALYZE"), "{f:?}");
        assert_eq!(index_fix(&my(Dialect::MySql, "mysql-orall-analyze", "orders", captured(Dialect::MySql))).as_deref(), Some("ALTER TABLE `shop`.`orders` ADD INDEX `orders_ref_idx` (`ref`), ALGORITHM=INPLACE, LOCK=NONE;"));
        assert_eq!(index_fix(&my(Dialect::MariaDb, "mariadb-orall-analyze", "orders", captured(Dialect::MariaDb))).as_deref(), Some("ALTER TABLE `shop`.`orders` ADD INDEX `orders_ref_idx` (`ref`), ALGORITHM=INPLACE, LOCK=NONE;"));
    }

    #[test]
    fn possible_keys_name_the_blocker_only_when_the_catalog_shows_it() {
        // Without the catalog, MySQL's possible keys and its guess are all there is.
        let p = parse(Dialect::MySql, fixture("mysql-orall")).unwrap();
        let f = unused(&p).unwrap();
        assert_eq!(f.body, ["Possible keys: idx_orders_email. The server judged a full scan cheaper, or the filter cannot use them.", "The server guesses the filter keeps 10% of the rows it reads."]);
    }

    #[test]
    fn a_join_buffer_on_an_unindexed_column_gets_an_index_when_few_rows_feed_it() {
        // MySQL's hash join: 39 customers at most look up their orders by customer_id.
        let mut t = table(Dialect::MySql, &[("", "o")], captured(Dialect::MySql));
        t.names = vec![(String::new(), "o".into())];
        let p = found(Dialect::MySql, "mysql-hashjoin", &ev(Dialect::MySql, t.clone()));
        let f = p.findings.iter().find(|f| f.kind == Kind::JoinBuffer).unwrap();
        assert_eq!(f.fix.as_ref().map(|x| x.sql.as_str()), Some("ALTER TABLE `shop`.`orders` ADD INDEX `orders_customer_id_idx` (`customer_id`), ALGORITHM=INPLACE, LOCK=NONE;"));
        assert!(f.evidence.iter().any(|l| l == "The tables before it give at most about 39 rows to look up, by the server's estimate."), "{:?}", f.evidence);
        // An index on the column already: nothing.
        let mut with = t.clone();
        with.indexes.as_mut().unwrap().push(ix("idx_cust", &["customer_id"]));
        assert!(found(Dialect::MySql, "mysql-hashjoin", &ev(Dialect::MySql, with)).findings.iter().all(|f| f.fix.as_ref().is_none_or(|x| x.kind != FixKind::Index)));
        // Thousands of orders feeding a scan of items: a hash join is the cheaper plan.
        let items = TableEv { names: vec![(String::new(), "i".into())], schema: "shop".into(), table: "items".into(), rows: Some(97_822.0), indexes: Some(vec![ix("PRIMARY", &["id"])]), cols: vec![col("sku", "varchar(20)")], ..Default::default() };
        let p = found(Dialect::MySql, "mysql-bufjoin", &ev(Dialect::MySql, items.clone()));
        assert_eq!(index_fix(&p), None);
        // MariaDB's join buffer step holds the condition; with few rows before it, the candidate.
        let orders = table(Dialect::MariaDb, &[("", "o")], captured(Dialect::MariaDb));
        assert_eq!(index_fix(&found(Dialect::MariaDb, "mariadb-bufjoin", &ev(Dialect::MariaDb, orders.clone()))), None);
        let mut p = parse(Dialect::MariaDb, &fixture("mariadb-bufjoin").replace("\"rows\": 100415", "\"rows\": 900")).unwrap();
        p.refind(Dialect::MariaDb, Some(&ev(Dialect::MariaDb, orders.clone())), false);
        assert_eq!(index_fix(&p).as_deref(), Some("ALTER TABLE `shop`.`orders` ADD INDEX `orders_ref_idx` (`ref`), ALGORITHM=INPLACE, LOCK=NONE;"));
        assert_eq!(trial_of(&p), None, "MySQL and MariaDB have no HypoPG");
        // The buffered table with no filter of its own, as `wants` now asks the catalog for it: the join's candidate,
        // and no word about a filter it does not have.
        let bare = fixture("mariadb-bufjoin").replace("\"rows\": 100415", "\"rows\": 900").replace(r#""o.`status` = 'paid' and o.total > 899""#, r#""""#);
        let mut p = parse(Dialect::MariaDb, &bare).unwrap();
        p.refind(Dialect::MariaDb, Some(&ev(Dialect::MariaDb, orders)), false);
        assert_eq!(index_fix(&p).as_deref(), Some("ALTER TABLE `shop`.`orders` ADD INDEX `orders_ref_idx` (`ref`), ALGORITHM=INPLACE, LOCK=NONE;"));
        assert!(p.findings.iter().all(|f| !f.no_fix.as_deref().unwrap_or_default().contains("filter could not be read")), "{:?}", p.findings);
    }

    fn rewritten(stmt: &str, d: Dialect, name: &str, alias: &str) -> Option<String> {
        let p = if d == Dialect::Postgres { pg(name, captured(d)) } else { my(d, name, alias, captured(d)) };
        let f = unused(&p)?;
        statement(stmt, d, &p.nodes[f.node?], f.fix.as_ref()?)
    }

    #[test]
    fn the_statement_is_rewritten_in_place_when_the_condition_is_found_once() {
        let pg = |s: &str, name: &str| rewritten(s, Dialect::Postgres, name, "");
        assert_eq!(pg("SELECT id FROM bookings WHERE created_at::date = '2026-03-01'", "pg18-datewrap").as_deref(), Some("SELECT id FROM bookings WHERE created_at >= '2026-03-01' AND created_at < '2026-03-02'"));
        assert_eq!(pg("select id from bookings where DATE(created_at) = DATE '2026-03-01' order by id;", "pg18-datefn").as_deref(), Some("select id from bookings where created_at >= '2026-03-01' AND created_at < '2026-03-02' order by id;"));
        assert_eq!(pg("SELECT id FROM bookings WHERE CAST(created_at AS date) = '2026-03-01'::date", "pg18-datewrap").as_deref(), Some("SELECT id FROM bookings WHERE created_at >= '2026-03-01' AND created_at < '2026-03-02'"));
        assert_eq!(pg("SELECT id FROM bookings WHERE ref = 'x' OR '2026-03-01' = created_at::date", "pg18-datewrap").as_deref(), Some("SELECT id FROM bookings WHERE ref = 'x' OR (created_at >= '2026-03-01' AND created_at < '2026-03-02')"));
        assert_eq!(pg("SELECT id FROM bookings WHERE id::text = '4242'", "pg18-cast").as_deref(), Some("SELECT id FROM bookings WHERE id = 4242"));
        assert_eq!(pg("SELECT b.id FROM bookings b WHERE (b.created_at::date = '2026-03-01')", "pg18-datewrap"), None, "the plan reads the table as bookings, not b");
        assert_eq!(pg("SELECT id FROM bookings WHERE (bookings.created_at::date = '2026-03-01')", "pg18-datewrap").as_deref(), Some("SELECT id FROM bookings WHERE (bookings.created_at >= '2026-03-01' AND bookings.created_at < '2026-03-02')"));
        let my = |s: &str, name: &str, alias: &str| rewritten(s, Dialect::MySql, name, alias);
        assert_eq!(
            my("SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = 'paid' AND DATE(o.created_at) = '2026-11-01'", "mysql-join", "o").as_deref(),
            Some("SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = 'paid' AND o.created_at >= '2026-11-01' AND o.created_at < '2026-11-02'")
        );
        assert_eq!(my("SELECT * FROM orders WHERE YEAR(`created_at`) = 2026 AND status = 'refunded'", "mysql-yearall", "orders").as_deref(), Some("SELECT * FROM orders WHERE `created_at` >= '2026-01-01' AND `created_at` < '2027-01-01' AND status = 'refunded'"));
        // Anything unsure leaves the statement alone: twice, a different value, a cast that continues, NOT, BETWEEN.
        for s in [
            "SELECT id FROM bookings WHERE created_at::date = '2026-03-01' OR created_at::date = '2026-03-01'",
            "SELECT id FROM bookings WHERE created_at::date = '2026-03-02'",
            "SELECT id FROM bookings WHERE created_at::date = '2026-03-01'::date + 0",
            "SELECT id FROM bookings WHERE NOT created_at::date = '2026-03-01'",
            "SELECT id FROM bookings WHERE id BETWEEN 1 AND 9 AND created_at::date = '2026-03-01'",
            "SELECT id FROM bookings WHERE x.created_at::date = '2026-03-01'",
            "SELECT id FROM bookings WHERE note = 'created_at::date = ''2026-03-01'''",
        ] {
            assert_eq!(pg(s, "pg18-datewrap"), None, "{s}");
        }
    }
}
