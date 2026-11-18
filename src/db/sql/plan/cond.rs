//! Reads the conditions a plan prints (PostgreSQL's Filter and Index Cond, MySQL's attached_condition) into a small tree,
//! so the dialog can list the columns a filter tests and lay it out one condition per line. A server prints its own
//! form of what the user typed (`= ANY ('{..}'::type[])` for IN, casts on literals, backticked full names), and those
//! are the forms read here. Anything else becomes `Other` with its text, so an odd part never loses the rest.

use super::super::lex::{self, Tok};
use super::super::Dialect;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Cmp { l: Term, op: String, r: Term },
    /// `IN (..)`, or PostgreSQL's `= ANY (array)`.
    In { l: Term, list: Vec<Term>, not: bool },
    Between { t: Term, lo: Term, hi: Term, not: bool },
    IsNull { t: Term, not: bool },
    /// LIKE, ILIKE and PostgreSQL's `~~` family.
    Like { t: Term, pat: Term, ci: bool, not: bool },
    /// A term used as a condition: a boolean column, a function that returns one, `(SubPlan 1)`.
    Bool(Term),
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Col(ColRef),
    /// A string, unquoted. Its cast, when the server wrote one, is in `Cast`.
    Str(String),
    Num(String),
    /// `$1`, `?` or a MySQL `@var`.
    Param(String),
    Func { name: String, args: Vec<Term> },
    Cast { inner: Box<Term>, ty: String },
    /// PostgreSQL's `(SubPlan 1)`, `(InitPlan 1).col1` and the like.
    Sub(String),
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColRef {
    /// The table or alias in front of the column, without the schema.
    pub qual: Option<String>,
    pub name: String,
}

impl ColRef {
    pub fn show(&self) -> String {
        match &self.qual {
            Some(q) => format!("{q}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// How a piece of a laid out condition is colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Col,
    Op,
    Kw,
    Val,
    Punct,
    /// "+5" for the values a long list folds away.
    More,
}

/// One line of a laid out condition: its indent in spaces and its pieces.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub indent: usize,
    pub parts: Vec<(Role, String)>,
}

/// The condition in `text`, or None when nothing in it reads as one.
pub fn parse(text: &str, d: Dialect) -> Option<Expr> {
    let toks = significant(text, d);
    if toks.is_empty() || toks.len() > MAX_TOKENS {
        return None;
    }
    let mut p = P { text, toks, i: 0, depth: 0 };
    let e = p.or()?;
    if p.i < p.toks.len() {
        return None;
    }
    match &e {
        Expr::Other(_) => None,
        _ => Some(e),
    }
}

/// The columns a condition tests, once each, in the order they appear.
pub fn columns(e: &Expr) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    walk_terms(e, &mut |t| {
        for c in term_cols(t) {
            if !out.contains(&c.name) {
                out.push(c.name.clone());
            }
        }
    });
    out
}

/// The condition laid out for reading: one top-level AND per line, literals without the casts the server added, long
/// lists cut to their first `fold` values and a count.
pub fn pretty(e: &Expr, fold: usize) -> Vec<Line> {
    let parts = match e {
        Expr::And(xs) => xs.iter().collect::<Vec<_>>(),
        _ => vec![e],
    };
    parts
        .into_iter()
        .enumerate()
        .map(|(i, x)| {
            let mut out = Vec::new();
            if i > 0 {
                out.push((Role::Kw, "AND ".to_string()));
            }
            show_expr(x, fold, false, &mut out);
            Line { indent: if i > 0 { 2 } else { 0 }, parts: out }
        })
        .collect()
}

/// The condition as one line of text, as `pretty` writes it.
pub fn one_line(e: &Expr, fold: usize) -> String {
    let mut out = Vec::new();
    show_expr(e, fold, false, &mut out);
    out.into_iter().map(|(_, s)| s).collect()
}

const MAX_TOKENS: usize = 20_000;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum K {
    Word,
    Str,
    Quoted,
    Op,
}

/// Tokens without spaces and comments, with the punctuation PostgreSQL and MySQL use for operators glued back together.
fn significant(text: &str, d: Dialect) -> Vec<(K, std::ops::Range<usize>)> {
    let mut out: Vec<(K, std::ops::Range<usize>)> = Vec::new();
    for (t, r) in lex::tokens(text, d) {
        let k = match t {
            Tok::Space | Tok::Comment | Tok::ExecOpen | Tok::ExecClose => continue,
            Tok::Word => K::Word,
            Tok::Str if text[r.clone()].starts_with('"') && d == Dialect::Postgres => K::Quoted,
            Tok::Str => K::Str,
            Tok::Quoted => K::Quoted,
            Tok::Punct => K::Op,
        };
        let s = &text[r.clone()];
        if k == K::Op && glues(s) {
            if let Some(last) = out.last_mut() {
                let prev = &text[last.1.clone()];
                if last.0 == K::Op && last.1.end == r.start && glues(prev) && glued(prev, s) {
                    last.1.end = r.end;
                    continue;
                }
            }
        }
        out.push((k, r));
    }
    out
}

fn glues(s: &str) -> bool {
    s.chars().all(|c| "<>=!~*:@".contains(c))
}

/// Whether `prev` and the next mark make one operator: `>=`, `<>`, `!=`, `::`, `~~*`, `!~~`, `@>`.
fn glued(prev: &str, next: &str) -> bool {
    let both = format!("{prev}{next}");
    matches!(both.as_str(), ">=" | "<=" | "<>" | "!=" | "::" | "~~" | "~~*" | "!~" | "!~~" | "!~~*" | "~*" | "!~*" | "<=>" | "@>" | "<@" | "==")
}

struct P<'a> {
    text: &'a str,
    toks: Vec<(K, std::ops::Range<usize>)>,
    i: usize,
    depth: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<(K, &str)> {
        self.toks.get(self.i).map(|(k, r)| (*k, &self.text[r.clone()]))
    }

    fn peek_at(&self, n: usize) -> Option<(K, &str)> {
        self.toks.get(self.i + n).map(|(k, r)| (*k, &self.text[r.clone()]))
    }

    fn is(&self, s: &str) -> bool {
        self.peek().is_some_and(|(_, t)| t.eq_ignore_ascii_case(s))
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.is(s) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn or(&mut self) -> Option<Expr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let mut xs = vec![self.and()?];
        while self.eat("or") {
            xs.push(self.and()?);
        }
        self.depth -= 1;
        Some(if xs.len() == 1 { xs.pop()? } else { Expr::Or(flat(xs, true)) })
    }

    fn and(&mut self) -> Option<Expr> {
        let mut xs = vec![self.not()?];
        while self.eat("and") {
            xs.push(self.not()?);
        }
        Some(if xs.len() == 1 { xs.pop()? } else { Expr::And(flat(xs, false)) })
    }

    fn not(&mut self) -> Option<Expr> {
        if self.is("not") && !self.peek_at(1).is_some_and(|(_, t)| ["in", "like", "ilike", "between"].iter().any(|w| t.eq_ignore_ascii_case(w))) {
            self.i += 1;
            return Some(Expr::Not(Box::new(self.not()?)));
        }
        Some(self.pred())
    }

    /// One condition. One that does not read runs to the next AND, OR or closing parenthesis at its level and is kept as text.
    fn pred(&mut self) -> Expr {
        let start = self.i;
        if let Some(e) = self.try_pred() {
            return e;
        }
        self.i = start;
        let mut depth = 0i32;
        while let Some((k, t)) = self.peek() {
            if k == K::Op && t == "(" {
                depth += 1;
            } else if k == K::Op && t == ")" {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            } else if depth == 0 && k == K::Word && (t.eq_ignore_ascii_case("and") || t.eq_ignore_ascii_case("or")) {
                break;
            }
            self.i += 1;
        }
        let (a, b) = (self.toks.get(start).map_or(self.text.len(), |t| t.1.start), self.toks.get(self.i.saturating_sub(1)).map_or(0, |t| t.1.end));
        Expr::Other(if b > a { self.text[a..b].to_string() } else { String::new() })
    }

    fn try_pred(&mut self) -> Option<Expr> {
        // A parenthesis opens either a group of conditions or a term: PostgreSQL wraps both, as in `((col)::date = ..)`.
        let left = if self.is("(") {
            self.i += 1;
            let inner = self.or()?;
            if !self.eat(")") {
                return None;
            }
            match inner {
                Expr::Bool(t) => self.postfix(t)?,
                e => return self.after_group(e),
            }
        } else {
            self.term()?
        };
        self.rest(left)
    }

    /// A group of conditions may still be compared, as in `(a = b) = true`; otherwise it stands alone.
    fn after_group(&mut self, e: Expr) -> Option<Expr> {
        match self.peek() {
            None => Some(e),
            Some((K::Op, ")")) => Some(e),
            Some((K::Word, w)) if w.eq_ignore_ascii_case("and") || w.eq_ignore_ascii_case("or") => Some(e),
            _ => None,
        }
    }

    /// What follows a term: a comparison, IN, BETWEEN, IS NULL or LIKE, or nothing when the term is the condition.
    fn rest(&mut self, l: Term) -> Option<Expr> {
        let Some((k, t)) = self.peek() else { return Some(Expr::Bool(l)) };
        let t = t.to_ascii_lowercase();
        if k == K::Op && matches!(t.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" | "<=>" | "==") {
            self.i += 1;
            let op = if t == "!=" { "<>".to_string() } else { t };
            if self.is("any") || self.is("some") || self.is("all") {
                let all = self.is("all");
                self.i += 1;
                if !self.eat("(") {
                    return None;
                }
                let arr = self.term()?;
                if !self.eat(")") {
                    return None;
                }
                if op == "=" && !all {
                    return Some(Expr::In { l, list: array_items(&arr), not: false });
                }
                if op == "<>" && all {
                    return Some(Expr::In { l, list: array_items(&arr), not: true });
                }
                return Some(Expr::Cmp { l, op, r: Term::Other(format!("{} (..)", if all { "ALL" } else { "ANY" })) });
            }
            let r = self.operand()?;
            return Some(Expr::Cmp { l, op, r });
        }
        if k == K::Op && matches!(t.as_str(), "~~" | "~~*" | "!~~" | "!~~*") {
            self.i += 1;
            let pat = self.operand()?;
            return Some(Expr::Like { t: l, pat, ci: t.ends_with('*'), not: t.starts_with('!') });
        }
        if k == K::Word {
            let not = t == "not";
            if not {
                self.i += 1;
            }
            let w = self.peek().map(|(_, s)| s.to_ascii_lowercase()).unwrap_or_default();
            match w.as_str() {
                "in" => {
                    self.i += 1;
                    if !self.eat("(") {
                        return None;
                    }
                    let mut list = vec![self.operand()?];
                    while self.eat(",") {
                        list.push(self.operand()?);
                    }
                    if !self.eat(")") {
                        return None;
                    }
                    return Some(Expr::In { l, list, not });
                }
                "between" => {
                    self.i += 1;
                    let lo = self.operand()?;
                    if !self.eat("and") {
                        return None;
                    }
                    let hi = self.operand()?;
                    return Some(Expr::Between { t: l, lo, hi, not });
                }
                "like" | "ilike" => {
                    self.i += 1;
                    let pat = self.operand()?;
                    return Some(Expr::Like { t: l, pat, ci: w == "ilike", not });
                }
                "is" if !not => {
                    self.i += 1;
                    let not = self.eat("not");
                    if self.eat("null") {
                        return Some(Expr::IsNull { t: l, not });
                    }
                    return None;
                }
                _ if not => return None,
                _ => {}
            }
        }
        Some(Expr::Bool(l))
    }

    /// A value on the right of an operator: a term, or a parenthesized one.
    fn operand(&mut self) -> Option<Term> {
        if self.is("(") {
            self.i += 1;
            let inner = self.or()?;
            if !self.eat(")") {
                return None;
            }
            let Expr::Bool(t) = inner else { return None };
            return self.postfix(t);
        }
        self.term()
    }

    fn term(&mut self) -> Option<Term> {
        let t = self.primary()?;
        self.postfix(t)
    }

    /// Casts and MySQL's `COLLATE` after a term.
    fn postfix(&mut self, mut t: Term) -> Option<Term> {
        loop {
            if self.eat("::") {
                let ty = self.type_name()?;
                t = Term::Cast { inner: Box::new(t), ty };
            } else if self.is("collate") {
                self.i += 1;
                self.i += 1;
            } else {
                return Some(t);
            }
        }
    }

    /// A type after `::` or `AS`: one or more words (`timestamp with time zone`), a size in parentheses and array brackets.
    fn type_name(&mut self) -> Option<String> {
        let mut s = String::new();
        let (k, w) = self.peek()?;
        if !matches!(k, K::Word | K::Quoted) {
            return None;
        }
        s.push_str(w);
        self.i += 1;
        while let Some((K::Word, w)) = self.peek() {
            if !["with", "without", "time", "zone", "varying", "precision", "unsigned", "signed"].iter().any(|x| w.eq_ignore_ascii_case(x)) {
                break;
            }
            s.push(' ');
            s.push_str(w);
            self.i += 1;
        }
        if self.is("(") {
            let start = self.i;
            while !self.is(")") && self.peek().is_some() {
                self.i += 1;
            }
            self.i += 1;
            s.push_str(&self.slice(start, self.i));
        }
        while self.is("[") && self.peek_at(1).is_some_and(|(_, t)| t == "]") {
            self.i += 2;
            s.push_str("[]");
        }
        Some(s)
    }

    fn slice(&self, a: usize, b: usize) -> String {
        match (self.toks.get(a), self.toks.get(b.saturating_sub(1))) {
            (Some(x), Some(y)) if y.1.end >= x.1.start => self.text[x.1.start..y.1.end].to_string(),
            _ => String::new(),
        }
    }

    fn primary(&mut self) -> Option<Term> {
        let (k, t) = self.peek()?;
        match k {
            K::Str => {
                let raw = t.to_string();
                self.i += 1;
                Some(Term::Str(unquote_str(&raw)))
            }
            K::Quoted => self.name(),
            K::Op if t == "-" || t == "+" => {
                let sign = t.to_string();
                self.i += 1;
                match self.primary()? {
                    Term::Num(n) => Some(Term::Num(format!("{}{n}", if sign == "-" { "-" } else { "" }))),
                    _ => None,
                }
            }
            K::Op if t == "?" => {
                self.i += 1;
                Some(Term::Param("?".into()))
            }
            K::Op if t == "(" => None,
            K::Op => None,
            K::Word => {
                let w = t.to_string();
                if w.starts_with('$') {
                    self.i += 1;
                    return Some(Term::Param(w));
                }
                if w.as_bytes()[0].is_ascii_digit() {
                    self.i += 1;
                    let mut n = w;
                    // 800.00 and 662e-6 arrive as several tokens.
                    if self.is(".") && self.peek_at(1).is_some_and(|(k, s)| k == K::Word && s.as_bytes()[0].is_ascii_digit()) {
                        n.push('.');
                        n.push_str(self.peek_at(1).map(|x| x.1).unwrap_or_default());
                        self.i += 2;
                    }
                    return Some(Term::Num(n));
                }
                let lw = w.to_ascii_lowercase();
                if matches!(lw.as_str(), "subplan" | "hashed" | "initplan") {
                    let start = self.i;
                    while self.peek().is_some_and(|(k, s)| k == K::Word || (k == K::Op && s == ".")) {
                        self.i += 1;
                    }
                    return Some(Term::Sub(self.slice(start, self.i)));
                }
                if matches!(lw.as_str(), "true" | "false" | "null") {
                    self.i += 1;
                    return Some(Term::Num(lw));
                }
                // MySQL writes literals with their type in front: TIMESTAMP'2026-03-01 00:00:00', DATE'..'.
                if matches!(lw.as_str(), "timestamp" | "date" | "time") && self.peek_at(1).is_some_and(|(k, _)| k == K::Str) {
                    self.i += 1;
                    let s = self.peek().map(|x| x.1.to_string()).unwrap_or_default();
                    self.i += 1;
                    return Some(Term::Cast { inner: Box::new(Term::Str(unquote_str(&s))), ty: lw });
                }
                if self.peek_at(1).is_some_and(|(k, s)| k == K::Op && s == "(") {
                    return self.call(lw);
                }
                if matches!(lw.as_str(), "and" | "or" | "not" | "is" | "in" | "like" | "between") {
                    return None;
                }
                self.name()
            }
        }
    }

    /// A function call. `cast(x as type)` becomes a cast, so `cast(col as date)` and `(col)::date` read the same.
    fn call(&mut self, name: String) -> Option<Term> {
        self.i += 2;
        if name == "cast" || name == "convert" {
            let inner = self.operand_or_expr()?;
            if !(self.eat("as") || self.eat(",")) {
                return None;
            }
            let ty = self.type_name()?;
            if !self.eat(")") {
                return None;
            }
            return Some(Term::Cast { inner: Box::new(inner), ty: ty.to_ascii_lowercase() });
        }
        let mut args = Vec::new();
        if !self.eat(")") {
            loop {
                if self.eat("*") {
                    args.push(Term::Other("*".into()));
                } else {
                    args.push(self.operand_or_expr()?);
                }
                if self.eat(")") {
                    break;
                }
                if !self.eat(",") {
                    return None;
                }
            }
        }
        // lcase is MariaDB's name for lower.
        let name = match name.as_str() {
            "lcase" => "lower".to_string(),
            "ucase" => "upper".to_string(),
            _ => name,
        };
        Some(Term::Func { name, args })
    }

    fn operand_or_expr(&mut self) -> Option<Term> {
        let start = self.i;
        if let Some(t) = self.operand() {
            if self.is(",") || self.is(")") || self.is("as") {
                return Some(t);
            }
        }
        // An argument that is itself an expression, such as `a + 1`: kept as text up to the next comma or parenthesis.
        self.i = start;
        let mut depth = 0i32;
        while let Some((k, t)) = self.peek() {
            if k == K::Op && t == "(" {
                depth += 1;
            } else if k == K::Op && t == ")" {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            } else if depth == 0 && ((k == K::Op && t == ",") || (k == K::Word && t.eq_ignore_ascii_case("as"))) {
                break;
            }
            self.i += 1;
        }
        if self.i == start {
            return None;
        }
        Some(Term::Other(self.slice(start, self.i)))
    }

    /// A column, possibly qualified: `col`, `b.col`, `"T"."col"` or MySQL's `shop`.`o`.`col`.
    fn name(&mut self) -> Option<Term> {
        let mut parts = Vec::new();
        loop {
            let (k, t) = self.peek()?;
            if !matches!(k, K::Word | K::Quoted) {
                return None;
            }
            parts.push(unquote_ident(t));
            self.i += 1;
            if !(self.is(".") && self.peek_at(1).is_some_and(|(k, _)| matches!(k, K::Word | K::Quoted))) {
                break;
            }
            self.i += 1;
        }
        let name = parts.pop()?;
        Some(Term::Col(ColRef { qual: parts.pop(), name }))
    }
}

/// A list nested inside a list of the same kind joins it, so `(a AND (b AND c))` reads as three conditions.
fn flat(xs: Vec<Expr>, or: bool) -> Vec<Expr> {
    let mut out = Vec::new();
    for x in xs {
        match x {
            Expr::Or(inner) if or => out.extend(inner),
            Expr::And(inner) if !or => out.extend(inner),
            x => out.push(x),
        }
    }
    out
}

fn unquote_str(s: &str) -> String {
    let q = s.chars().next().unwrap_or('\'');
    let inner = s.strip_prefix(q).and_then(|s| s.strip_suffix(q)).unwrap_or(s);
    inner.replace(&format!("{q}{q}"), &q.to_string())
}

fn unquote_ident(s: &str) -> String {
    for q in ['`', '"'] {
        if let Some(inner) = s.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return inner.replace(&format!("{q}{q}"), &q.to_string());
        }
    }
    s.to_string()
}

/// The values of an array term: PostgreSQL's `'{A,B}'::type[]` or `ARRAY[..]`.
fn array_items(t: &Term) -> Vec<Term> {
    let base = match t {
        Term::Cast { inner, .. } => inner.as_ref(),
        t => t,
    };
    match base {
        Term::Str(s) if s.starts_with('{') && s.ends_with('}') => split_array(&s[1..s.len() - 1]).into_iter().map(Term::Str).collect(),
        t => vec![t.clone()],
    }
}

/// The items of an array literal's body: commas split, double quotes group, a backslash escapes.
fn split_array(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut esc) = (false, false);
    for c in s.chars() {
        if esc {
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == ',' && !quoted {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() || !out.is_empty() {
        out.push(cur);
    }
    out
}

/// The columns a term reads, through functions and casts.
pub fn term_cols(t: &Term) -> Vec<&ColRef> {
    match t {
        Term::Col(c) => vec![c],
        Term::Cast { inner, .. } => term_cols(inner),
        Term::Func { args, .. } => args.iter().flat_map(term_cols).collect(),
        _ => Vec::new(),
    }
}

/// Calls `f` on every term at the top of each condition in `e`.
pub fn walk_terms<'a>(e: &'a Expr, f: &mut impl FnMut(&'a Term)) {
    match e {
        Expr::And(xs) | Expr::Or(xs) => xs.iter().for_each(|x| walk_terms(x, f)),
        Expr::Not(x) => walk_terms(x, f),
        Expr::Cmp { l, r, .. } => {
            f(l);
            f(r);
        }
        Expr::In { l, list, .. } => {
            f(l);
            list.iter().for_each(|x| f(x));
        }
        Expr::Between { t, lo, hi, .. } => {
            f(t);
            f(lo);
            f(hi);
        }
        Expr::IsNull { t, .. } | Expr::Bool(t) => f(t),
        Expr::Like { t, pat, .. } => {
            f(t);
            f(pat);
        }
        Expr::Other(_) => {}
    }
}

fn show_expr(e: &Expr, fold: usize, nested: bool, out: &mut Vec<(Role, String)>) {
    let kw = |out: &mut Vec<(Role, String)>, s: &str| out.push((Role::Kw, s.to_string()));
    let op = |out: &mut Vec<(Role, String)>, s: &str| out.push((Role::Op, s.to_string()));
    match e {
        Expr::And(xs) | Expr::Or(xs) => {
            let w = if matches!(e, Expr::And(_)) { " AND " } else { " OR " };
            if nested {
                out.push((Role::Punct, "(".into()));
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    kw(out, w);
                }
                show_expr(x, fold, true, out);
            }
            if nested {
                out.push((Role::Punct, ")".into()));
            }
        }
        Expr::Not(x) => {
            kw(out, "NOT ");
            show_expr(x, fold, true, out);
        }
        Expr::Cmp { l, op: o, r } => {
            show_term(l, out);
            op(out, &format!(" {o} "));
            show_term(r, out);
        }
        Expr::In { l, list, not } => {
            show_term(l, out);
            kw(out, if *not { " NOT IN " } else { " IN " });
            out.push((Role::Punct, "(".into()));
            for (i, x) in list.iter().take(fold.max(1)).enumerate() {
                if i > 0 {
                    out.push((Role::Punct, ", ".into()));
                }
                show_term(x, out);
            }
            if list.len() > fold.max(1) {
                out.push((Role::More, format!(" +{}", list.len() - fold.max(1))));
            }
            out.push((Role::Punct, ")".into()));
        }
        Expr::Between { t, lo, hi, not } => {
            show_term(t, out);
            kw(out, if *not { " NOT BETWEEN " } else { " BETWEEN " });
            show_term(lo, out);
            kw(out, " AND ");
            show_term(hi, out);
        }
        Expr::IsNull { t, not } => {
            show_term(t, out);
            kw(out, if *not { " IS NOT NULL" } else { " IS NULL" });
        }
        Expr::Like { t, pat, ci, not } => {
            show_term(t, out);
            kw(out, &format!(" {}{} ", if *not { "NOT " } else { "" }, if *ci { "ILIKE" } else { "LIKE" }));
            show_term(pat, out);
        }
        Expr::Bool(t) => show_term(t, out),
        Expr::Other(s) => out.push((Role::Punct, s.clone())),
    }
}

fn show_term(t: &Term, out: &mut Vec<(Role, String)>) {
    match t {
        Term::Col(c) => out.push((Role::Col, c.show())),
        Term::Str(s) => out.push((Role::Val, format!("'{}'", s.replace('\'', "''")))),
        Term::Num(n) => out.push((Role::Val, n.clone())),
        Term::Param(p) => out.push((Role::Val, p.clone())),
        // A cast the server put on a literal says nothing the reader needs; one on a column matters.
        Term::Cast { inner, ty } => match inner.as_ref() {
            Term::Str(_) | Term::Num(_) => show_term(inner, out),
            i => {
                show_term(i, out);
                out.push((Role::Op, format!("::{ty}")));
            }
        },
        Term::Func { name, args } => {
            out.push((Role::Kw, name.clone()));
            out.push((Role::Punct, "(".into()));
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push((Role::Punct, ", ".into()));
                }
                show_term(a, out);
            }
            out.push((Role::Punct, ")".into()));
        }
        Term::Sub(s) => out.push((Role::Kw, format!("({s})"))),
        Term::Other(s) => out.push((Role::Punct, s.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg(s: &str) -> Expr {
        parse(s, Dialect::Postgres).unwrap_or_else(|| panic!("{s} did not parse"))
    }

    fn my(s: &str) -> Expr {
        parse(s, Dialect::MySql).unwrap_or_else(|| panic!("{s} did not parse"))
    }

    fn col(n: &str) -> Term {
        Term::Col(ColRef { qual: None, name: n.into() })
    }

    #[test]
    fn postgres_forms_from_real_plans() {
        let e = pg("((provider = ANY ('{ZEUS_TAXI,ZEUS_VLOC,ZEUS_TRAIN}'::provider[])) AND (status = ANY ('{CONFIRMED,CANCELLED,FAILED}'::booking_status[])))");
        let Expr::And(xs) = &e else { panic!("{e:?}") };
        assert_eq!(xs[0], Expr::In { l: col("provider"), list: vec![Term::Str("ZEUS_TAXI".into()), Term::Str("ZEUS_VLOC".into()), Term::Str("ZEUS_TRAIN".into())], not: false });
        assert_eq!(columns(&e), ["provider", "status"]);
        let e = pg("((created_at >= '2026-10-12 22:00:00+00'::timestamp with time zone) AND (created_at < '2026-11-13 23:00:00+00'::timestamp with time zone))");
        assert_eq!(columns(&e), ["created_at"]);
        assert_eq!(one_line(&e, 3), "created_at >= '2026-10-12 22:00:00+00' AND created_at < '2026-11-13 23:00:00+00'");
        // A cast on the column is what makes an index on it useless, so it stays.
        assert_eq!(pg("((created_at)::date = '2026-03-01'::date)"), Expr::Cmp { l: Term::Cast { inner: Box::new(col("created_at")), ty: "date".into() }, op: "=".into(), r: Term::Cast { inner: Box::new(Term::Str("2026-03-01".into())), ty: "date".into() } });
        assert!(matches!(pg("(date(created_at) = '2026-03-01'::date)"), Expr::Cmp { l: Term::Func { ref name, .. }, .. } if name == "date"));
        assert!(matches!(pg("(lower((email)::text) = 'user42@example.com'::text)"), Expr::Cmp { l: Term::Func { ref name, .. }, .. } if name == "lower"));
        assert_eq!(pg("((email)::text ~~ '%42@example.com'::text)"), Expr::Like { t: Term::Cast { inner: Box::new(col("email")), ty: "text".into() }, pat: Term::Cast { inner: Box::new(Term::Str("%42@example.com".into())), ty: "text".into() }, ci: false, not: false });
        assert!(matches!(pg("(((email)::text = 'user42@example.com'::text) OR (ref = 'R4242'::text))"), Expr::Or(ref xs) if xs.len() == 2));
        let e = pg("((email IS NOT NULL) AND (NOT flagged) AND (total >= '10'::numeric) AND (total <= '20'::numeric) AND (ref <> 'R1'::text))");
        let Expr::And(xs) = &e else { panic!() };
        assert_eq!(xs[0], Expr::IsNull { t: col("email"), not: true });
        assert_eq!(xs[1], Expr::Not(Box::new(Expr::Bool(col("flagged")))));
        assert_eq!(columns(&e), ["email", "flagged", "total", "ref"]);
        assert_eq!(pg("(b.service_id = si.service_id)"), Expr::Cmp { l: Term::Col(ColRef { qual: Some("b".into()), name: "service_id".into() }), op: "=".into(), r: Term::Col(ColRef { qual: Some("si".into()), name: "service_id".into() }) });
        assert_eq!(pg("(id = 3)"), Expr::Cmp { l: col("id"), op: "=".into(), r: Term::Num("3".into()) });
        assert_eq!(pg("(id = $1)"), Expr::Cmp { l: col("id"), op: "=".into(), r: Term::Param("$1".into()) });
        assert!(matches!(pg("(NOT (hashed SubPlan 1))"), Expr::Not(_)));
    }

    #[test]
    fn mysql_and_mariadb_forms_from_real_plans() {
        let e = my("((`shop`.`o`.`status` = 'paid') and (cast(`shop`.`o`.`created_at` as date) = '2026-11-01'))");
        let Expr::And(xs) = &e else { panic!() };
        assert_eq!(xs[1], Expr::Cmp { l: Term::Cast { inner: Box::new(Term::Col(ColRef { qual: Some("o".into()), name: "created_at".into() })), ty: "date".into() }, op: "=".into(), r: Term::Str("2026-11-01".into()) });
        assert_eq!(columns(&e), ["status", "created_at"]);
        assert_eq!(one_line(&my("((`shop`.`orders`.`status` = 'failed') and (`shop`.`orders`.`total` > 800.00))"), 3), "orders.status = 'failed' AND orders.total > 800.00");
        assert!(matches!(my("(year(`shop`.`orders`.`created_at`) = 2026)"), Expr::Cmp { l: Term::Func { ref name, .. }, .. } if name == "year"));
        assert!(matches!(my("(`shop`.`orders`.`email` like '%42@example.com')"), Expr::Like { ci: false, not: false, .. }));
        assert!(matches!(my("((`shop`.`orders`.`created_at` >= TIMESTAMP'2026-03-01 00:00:00') and (`shop`.`orders`.`created_at` < TIMESTAMP'2026-03-02 00:00:00'))"), Expr::And(_)));
        let maria = parse("orders.created_at between '2026-03-01 00:00:00' and '2026-03-01 23:59:59' and orders.`status` = 'refunded'", Dialect::MariaDb).unwrap();
        let Expr::And(xs) = &maria else { panic!("{maria:?}") };
        assert!(matches!(xs[0], Expr::Between { .. }));
        assert!(matches!(parse("lcase(orders.email) = 'user42@example.com'", Dialect::MariaDb), Some(Expr::Cmp { l: Term::Func { ref name, .. }, .. }) if name == "lower"));
        assert_eq!(columns(&parse("o.customer_id = c.`id` and o.`status` = 'paid'", Dialect::MariaDb).unwrap()), ["customer_id", "id", "status"]);
    }

    #[test]
    fn lists_fold_and_lines_split_at_and() {
        let e = pg("((provider = ANY ('{A,B,C,D,E}'::p[])) AND (total > '5'::numeric))");
        let lines = pretty(&e, 3);
        assert_eq!(lines.len(), 2);
        let text = |l: &Line| l.parts.iter().map(|p| p.1.as_str()).collect::<String>();
        assert_eq!(text(&lines[0]), "provider IN ('A', 'B', 'C' +2)");
        assert_eq!((lines[1].indent, text(&lines[1])), (2, "AND total > '5'".to_string()));
    }

    #[test]
    fn what_does_not_read_stays_as_text() {
        // CASE is not read, but the condition after it still is.
        let e = pg("((CASE WHEN (a > 1) THEN 1 ELSE 2 END = 1) AND (b = 2))");
        assert!(columns(&e).contains(&"b".to_string()), "{e:?}");
        for junk in ["", "(((", ")))", "= = =", "'unterminated", "a AND", "NOT", "x IN (", "::", "cast(a as)"] {
            let _ = parse(junk, Dialect::Postgres);
            let _ = parse(junk, Dialect::MySql);
        }
        let deep = format!("{}a = 1{}", "(".repeat(500), ")".repeat(500));
        assert!(parse(&deep, Dialect::Postgres).is_none());
    }
}
