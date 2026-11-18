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
        // MySQL prints `cast(id as char charset latin1)`; the character set does not change which rows match.
        if self.eat("charset") || (self.is("character") && self.peek_at(1).is_some_and(|(_, t)| t.eq_ignore_ascii_case("set")) && self.eat("character") && self.eat("set")) {
            self.i += 1;
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
            parts.push(lex::unquote_ident(t));
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

/// A quoted string's text, the doubled quote inside undone.
pub(super) fn unquote_str(s: &str) -> String {
    let q = s.chars().next().unwrap_or('\'');
    let inner = s.strip_prefix(q).and_then(|s| s.strip_suffix(q)).unwrap_or(s);
    inner.replace(&format!("{q}{q}"), &q.to_string())
}

/// PostgreSQL's names for text, which a varchar column is relabeled to without a real cast.
pub(super) fn text_type(ty: &str) -> bool {
    matches!(ty, "text" | "character varying" | "varchar")
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
pub(super) fn term_cols(t: &Term) -> Vec<&ColRef> {
    match t {
        Term::Col(c) => vec![c],
        Term::Cast { inner, .. } => term_cols(inner),
        Term::Func { args, .. } => args.iter().flat_map(term_cols).collect(),
        _ => Vec::new(),
    }
}

/// Calls `f` on every term at the top of each condition in `e`.
pub(super) fn walk_terms<'a>(e: &'a Expr, f: &mut impl FnMut(&'a Term)) {
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

/// How a condition on a column can narrow an index search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SargKind {
    /// `col = v`, `col IS NULL`, a boolean column.
    Eq,
    /// `col IN (..)`, PostgreSQL's `= ANY (..)`.
    In,
    /// `<`, `>`, BETWEEN.
    Range,
    /// `LIKE 'abc%'`, which only MySQL and MariaDB search an ordinary index with.
    Prefix,
}

/// A value a condition compares a column with.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Lit(String),
    Null,
    /// A parameter, an outer column or an expression: one value, not known here.
    Unknown,
}

/// One top-level condition an index on `col` could serve.
#[derive(Clone, Debug, PartialEq)]
pub struct Sarg {
    pub col: String,
    pub kind: SargKind,
    pub values: Vec<Val>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// A function around the column: `date(created_at)`, `lower(email)`.
    Wrapped,
    /// A cast on the column to another type.
    Cast,
    /// A LIKE pattern that starts with a wildcard.
    LeadingWild,
    /// The column is one side of an OR.
    Or,
}

/// A condition on `col` written so an index on the column cannot serve it.
#[derive(Clone, Debug, PartialEq)]
pub struct Blocker {
    pub col: String,
    pub kind: BlockKind,
    /// The function or type, "date" or "text"; empty for the others.
    pub what: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sargs {
    pub sargs: Vec<Sarg>,
    pub blockers: Vec<Blocker>,
    /// Top-level conditions that are neither: `<>`, NOT IN, a comparison of two columns, text that did not read.
    pub other: usize,
}

/// What an index could do with the top-level AND of a filter on one table. `own` says whether a column belongs to the
/// table scanned; with `outer_const` a column of another table counts as a value, as on the inner side of a nested loop
/// where it is fixed for each loop. `ty` gives a column's catalog type: PostgreSQL prints a varchar column compared with
/// text as `(col)::text`, a cast that an index on the column still serves, and only the type tells that cast apart.
pub(super) fn sargs(e: &Expr, d: Dialect, own: &dyn Fn(&ColRef) -> bool, outer_const: bool, ty: &dyn Fn(&str) -> Option<String>) -> Sargs {
    let mut out = Sargs::default();
    let parts = match e {
        Expr::And(xs) => xs.iter().collect::<Vec<_>>(),
        _ => vec![e],
    };
    let c = Cx { d, own, outer_const, ty };
    for x in parts {
        c.conjunct(x, &mut out);
    }
    out
}

/// How a term reaches its column.
enum Reach<'a> {
    Plain(&'a ColRef),
    Cast(&'a ColRef, &'a str),
    Wrapped(&'a ColRef, &'a str),
}

struct Cx<'a> {
    d: Dialect,
    own: &'a dyn Fn(&ColRef) -> bool,
    outer_const: bool,
    ty: &'a dyn Fn(&str) -> Option<String>,
}

/// SQL words PostgreSQL prints bare that are values, not columns.
const VALUE_WORDS: &[&str] = &["current_date", "current_timestamp", "current_time", "localtimestamp", "localtime"];

impl Cx<'_> {
    fn conjunct(&self, x: &Expr, out: &mut Sargs) {
        match x {
            Expr::Cmp { l, op, r } => {
                let (reach, other, op) = match (self.reach(l), self.reach(r)) {
                    (Some(a), None) if self.constant(r) => (a, r, op.clone()),
                    (None, Some(b)) if self.constant(l) => (b, l, flip(op)),
                    _ => return out.other += 1,
                };
                let kind = match op.as_str() {
                    "=" | "<=>" | "==" => SargKind::Eq,
                    "<" | "<=" | ">" | ">=" => SargKind::Range,
                    _ => return out.other += 1,
                };
                self.push(reach, kind, vec![val(other)], out);
            }
            Expr::In { l, list, not: false } if list.iter().all(|t| self.constant(t)) => match self.reach(l) {
                Some(r) => {
                    let kind = if list.len() == 1 { SargKind::Eq } else { SargKind::In };
                    self.push(r, kind, list.iter().map(val).collect(), out);
                }
                None => out.other += 1,
            },
            Expr::Between { t, lo, hi, not: false } if self.constant(lo) && self.constant(hi) => match self.reach(t) {
                Some(r) => self.push(r, SargKind::Range, Vec::new(), out),
                None => out.other += 1,
            },
            Expr::IsNull { t, not: false } => match self.reach(t) {
                Some(r) => self.push(r, SargKind::Eq, vec![Val::Null], out),
                None => out.other += 1,
            },
            Expr::Like { t, pat, ci, not: false } => {
                let Some(r) = self.reach(t) else { return out.other += 1 };
                let p = match val(pat) {
                    Val::Lit(p) => p,
                    _ => return out.other += 1,
                };
                if p.starts_with(['%', '_']) {
                    if let Reach::Plain(c) | Reach::Cast(c, _) = r {
                        out.blockers.push(Blocker { col: c.name.clone(), kind: BlockKind::LeadingWild, what: String::new() });
                        return;
                    }
                }
                // PostgreSQL searches a B-tree with a LIKE prefix only under the C collation or a pattern operator class,
                // which the catalog read here does not show, so there the prefix is no search.
                match r {
                    Reach::Plain(_) if *ci || self.d == Dialect::Postgres => out.other += 1,
                    r => self.push(r, SargKind::Prefix, Vec::new(), out),
                }
            }
            Expr::Bool(t) => match self.reach(t) {
                Some(r) => self.push(r, SargKind::Eq, vec![Val::Lit("true".into())], out),
                None => out.other += 1,
            },
            Expr::Not(inner) => match inner.as_ref() {
                Expr::Bool(t) => match self.reach(t) {
                    Some(r) => self.push(r, SargKind::Eq, vec![Val::Lit("false".into())], out),
                    None => out.other += 1,
                },
                _ => out.other += 1,
            },
            Expr::Or(xs) => {
                let mut seen: Vec<String> = Vec::new();
                for x in xs {
                    walk_terms(x, &mut |t| {
                        for c in term_cols(t) {
                            if (self.own)(c) && !self.value_word(c) && !seen.contains(&c.name) {
                                seen.push(c.name.clone());
                            }
                        }
                    });
                }
                if seen.is_empty() {
                    out.other += 1;
                }
                out.blockers.extend(seen.into_iter().map(|col| Blocker { col, kind: BlockKind::Or, what: String::new() }));
            }
            _ => out.other += 1,
        }
    }

    /// A sarg for a plain column; a blocker for a cast or a function around it.
    fn push(&self, r: Reach, kind: SargKind, values: Vec<Val>, out: &mut Sargs) {
        match r {
            Reach::Plain(c) => out.sargs.push(Sarg { col: c.name.clone(), kind, values }),
            Reach::Cast(c, t) => out.blockers.push(Blocker { col: c.name.clone(), kind: BlockKind::Cast, what: t.to_string() }),
            Reach::Wrapped(c, f) => out.blockers.push(Blocker { col: c.name.clone(), kind: BlockKind::Wrapped, what: f.to_string() }),
        }
    }

    fn value_word(&self, c: &ColRef) -> bool {
        c.qual.is_none() && VALUE_WORDS.contains(&c.name.to_ascii_lowercase().as_str())
    }

    fn mine(&self, c: &ColRef) -> bool {
        (self.own)(c) && !self.value_word(c)
    }

    /// The column of this table a term reads, and what is around it.
    fn reach<'t>(&self, t: &'t Term) -> Option<Reach<'t>> {
        match t {
            Term::Col(c) if self.mine(c) => Some(Reach::Plain(c)),
            Term::Cast { inner, ty } => match inner.as_ref() {
                Term::Col(c) if self.mine(c) => Some(if self.relabel(c, ty) { Reach::Plain(c) } else { Reach::Cast(c, ty) }),
                Term::Func { name, args } => args.iter().flat_map(term_cols).find(|c| self.mine(c)).map(|c| Reach::Wrapped(c, name.as_str())),
                _ => None,
            },
            Term::Func { name, args } => args.iter().flat_map(term_cols).find(|c| self.mine(c)).map(|c| Reach::Wrapped(c, name.as_str())),
            _ => None,
        }
    }

    /// PostgreSQL's `(col)::text` on a varchar column: a relabel the index on the column still serves. char to text strips
    /// the padding and citext to text changes the comparison, so those are real casts an index on the column cannot serve.
    fn relabel(&self, c: &ColRef, ty: &str) -> bool {
        if self.d != Dialect::Postgres || !text_type(ty) {
            return false;
        }
        (self.ty)(&c.name).is_some_and(|t| {
            let t = t.to_ascii_lowercase();
            t.starts_with("character varying") || t.starts_with("varchar")
        })
    }

    /// A value fixed for the scan: a literal, a parameter, a subplan's result, a function of those, and with
    /// `outer_const` a column of another table.
    fn constant(&self, t: &Term) -> bool {
        match t {
            Term::Str(_) | Term::Num(_) | Term::Param(_) | Term::Sub(_) => true,
            Term::Cast { inner, .. } => self.constant(inner),
            Term::Func { args, .. } => args.iter().all(|a| self.constant(a)),
            Term::Col(c) => self.value_word(c) || (self.outer_const && !(self.own)(c)),
            Term::Other(_) => false,
        }
    }
}

/// `5 < col` read as `col > 5`.
pub(super) fn flip(op: &str) -> String {
    match op {
        "<" => ">",
        ">" => "<",
        "<=" => ">=",
        ">=" => "<=",
        o => o,
    }
    .to_string()
}

fn val(t: &Term) -> Val {
    match t {
        Term::Str(s) => Val::Lit(s.clone()),
        Term::Num(n) if n == "null" => Val::Null,
        Term::Num(n) => Val::Lit(n.clone()),
        Term::Cast { inner, .. } => val(inner),
        _ => Val::Unknown,
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
        // The character set of a cast to char is read past.
        assert!(matches!(my("(cast(`shop`.`orders`.`id` as char charset latin1) = '4242')"), Expr::Cmp { l: Term::Cast { ref ty, .. }, .. } if ty == "char"));
        assert!(matches!(parse("cast(orders.`id` as char character set utf8mb3) = '4242'", Dialect::MariaDb), Some(Expr::Cmp { l: Term::Cast { ref ty, .. }, .. }) if ty == "char"));
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

    fn kinds(s: &Sargs) -> Vec<(&str, SargKind)> {
        s.sargs.iter().map(|g| (g.col.as_str(), g.kind)).collect()
    }

    fn blocks(s: &Sargs) -> Vec<(&str, BlockKind, &str)> {
        s.blockers.iter().map(|b| (b.col.as_str(), b.kind, b.what.as_str())).collect()
    }

    #[test]
    fn sargs_from_real_filters() {
        let any = |_: &ColRef| true;
        let varchar = |c: &str| (c == "email").then(|| "character varying(120)".to_string());
        let pg_s = |t: &str| sargs(&pg(t), Dialect::Postgres, &any, false, &varchar);
        let s = pg_s("((created_at >= '2026-10-12 22:00:00+00'::timestamp with time zone) AND (provider = ANY ('{ZEUS_TAXI,ZEUS_VLOC}'::provider[])) AND (status = 'DONE'::booking_status) AND (email IS NULL) AND (NOT flagged))");
        assert_eq!(kinds(&s), [("created_at", SargKind::Range), ("provider", SargKind::In), ("status", SargKind::Eq), ("email", SargKind::Eq), ("flagged", SargKind::Eq)]);
        assert_eq!(s.sargs[2].values, [Val::Lit("DONE".into())]);
        assert_eq!((s.sargs[3].values.clone(), s.sargs[4].values.clone()), (vec![Val::Null], vec![Val::Lit("false".into())]));
        // A varchar column relabeled to text is still the column; a real cast or a function is not.
        assert_eq!(kinds(&pg_s("((email)::text = 'user42@example.com'::text)")), [("email", SargKind::Eq)]);
        assert_eq!(blocks(&pg_s("((created_at)::date = '2026-03-01'::date)")), [("created_at", BlockKind::Cast, "date")]);
        assert_eq!(blocks(&pg_s("(lower((email)::text) = 'user42@example.com'::text)")), [("email", BlockKind::Wrapped, "lower")]);
        assert_eq!(blocks(&pg_s("((email)::text ~~ '%42@example.com'::text)")), [("email", BlockKind::LeadingWild, "")]);
        // A prefix LIKE searches no ordinary PostgreSQL index; MySQL reads it as a range.
        let s = pg_s("((email)::text ~~ 'user42%'::text)");
        assert!(s.sargs.is_empty() && s.blockers.is_empty() && s.other == 1, "{s:?}");
        let my_s = |t: &str| sargs(&my(t), Dialect::MySql, &any, false, &|_| None);
        assert_eq!(kinds(&my_s("(`shop`.`orders`.`email` like 'user42%')")), [("email", SargKind::Prefix)]);
        assert_eq!(blocks(&my_s("(year(`shop`.`orders`.`created_at`) = 2026)")), [("created_at", BlockKind::Wrapped, "year")]);
        let s = pg_s("(((email)::text = 'user42@example.com'::text) OR (ref = 'R4242'::text))");
        assert_eq!(blocks(&s), [("email", BlockKind::Or, ""), ("ref", BlockKind::Or, "")]);
        // `<>`, NOT IN and a comparison of two columns are no search.
        assert_eq!(pg_s("((ref <> 'R1'::text) AND (total > cap))").other, 2);
        // Another table's column is a value only for the inner side of a nested loop.
        let mine = |c: &ColRef| c.qual.as_deref() == Some("b");
        let e = pg("(b.service_id = s.id)");
        assert!(sargs(&e, Dialect::Postgres, &mine, false, &|_| None).sargs.is_empty());
        assert_eq!(kinds(&sargs(&e, Dialect::Postgres, &mine, true, &|_| None)), [("service_id", SargKind::Eq)]);
        // `5 < total` reads as `total > 5`.
        assert_eq!(kinds(&pg_s("('5'::numeric < total)")), [("total", SargKind::Range)]);
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
