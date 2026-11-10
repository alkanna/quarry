//! Typed filter rules of the Content bar: the operators per column type (Sequel Ace's lists), the
//! SQL a rule list turns into, and the way back for text this module wrote itself.

use super::value::{lit, Kind};
use super::Dialect;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    In,
    NotIn,
    NotInOrNull,
    Between,
    IsNull,
    NotNull,
    Contains,
    NotContains,
    StartsWith,
    NotStartsWith,
    EndsWith,
    NotEndsWith,
    Like,
    NotLike,
    Regexp,
    NotRegexp,
    Empty,
    NotEmpty,
    LenEq,
    LenGt,
    LenLt,
    Today,
    Last,
    ThisWeek,
    ThisMonth,
    AfterCol,
    SetContains,
    JsonPathEq,
    JsonContains,
    JsonHasPath,
}

/// The column groups that have their own operator list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Number,
    Text,
    Date,
    Choice,
    Other,
}

pub fn group_of(kind: Kind) -> Group {
    match kind {
        Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal | Kind::Year | Kind::Time => Group::Number,
        Kind::Text => Group::Text,
        Kind::Date | Kind::DateTime => Group::Date,
        Kind::Enum | Kind::Set => Group::Choice,
        Kind::Binary | Kind::Json | Kind::Bit | Kind::Geometry => Group::Other,
    }
}

/// What a value editor has to offer for an operator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input {
    None,
    One,
    Two,
    List,
    /// A count and a unit (hours, days, weeks, months).
    Last,
    /// A JSON path and a value.
    Path,
    Column,
}

pub const UNITS: [&str; 4] = ["hours", "days", "weeks", "months"];

/// One rule. `kind` decides how values are written (only number columns get bare numbers).
#[derive(Clone, PartialEq, Debug)]
pub struct Rule {
    pub col: String,
    pub kind: Kind,
    pub op: Op,
    pub values: Vec<String>,
}

impl Rule {
    pub fn new(col: &str, kind: Kind) -> Rule {
        let op = ops_for(kind)[0];
        Rule { col: col.into(), kind, op, values: default_values(op) }
    }

    /// Whether the rule has all the values its operator needs.
    pub fn complete(&self) -> bool {
        if self.col.is_empty() {
            return false;
        }
        let n = self.values.iter().filter(|v| !v.trim().is_empty()).count();
        match input(self.op) {
            Input::None => true,
            Input::One | Input::Column => n >= 1,
            Input::Two | Input::Last | Input::Path => n >= 2,
            Input::List => n >= 1,
        }
    }
}

pub fn input(op: Op) -> Input {
    use Op::*;
    match op {
        IsNull | NotNull | Empty | NotEmpty | Today | ThisWeek | ThisMonth => Input::None,
        Between => Input::Two,
        In | NotIn | NotInOrNull => Input::List,
        Last => Input::Last,
        JsonPathEq => Input::Path,
        AfterCol => Input::Column,
        _ => Input::One,
    }
}

pub fn default_values(op: Op) -> Vec<String> {
    match input(op) {
        Input::None => Vec::new(),
        Input::One | Input::List | Input::Column => vec![String::new()],
        Input::Two | Input::Path => vec![String::new(), String::new()],
        Input::Last => vec!["7".into(), "days".into()],
    }
}

/// The operators of a column type, in menu order.
pub fn ops_for(kind: Kind) -> &'static [Op] {
    use Op::*;
    match group_of(kind) {
        Group::Number => &[Eq, Ne, Gt, Lt, Ge, Le, In, NotInOrNull, Between, IsNull, NotNull],
        Group::Text => &[Eq, Ne, Contains, NotContains, StartsWith, NotStartsWith, EndsWith, NotEndsWith, Like, NotLike, Regexp, NotRegexp, In, NotInOrNull, Empty, NotEmpty, IsNull, NotNull, LenEq, LenGt, LenLt],
        Group::Date => &[Eq, Ne, Lt, Gt, Between, Today, Last, ThisWeek, ThisMonth, AfterCol, IsNull, NotNull],
        Group::Choice => &[Eq, Ne, In, NotIn, SetContains, IsNull, NotNull],
        Group::Other => &[Eq, Ne, LenEq, JsonPathEq, JsonContains, JsonHasPath, IsNull, NotNull],
    }
}

/// The menu text of an operator for a column type.
pub fn label(op: Op, kind: Kind) -> &'static str {
    use Op::*;
    let g = group_of(kind);
    match op {
        Eq if g == Group::Choice => "is",
        Ne if g == Group::Choice => "is not",
        Eq if g == Group::Other => "= (hex string)",
        Ne if g == Group::Other => "≠ (hex string)",
        Eq => "=",
        Ne => "≠",
        Gt if g == Group::Date => "is after",
        Lt if g == Group::Date => "is before",
        Gt => ">",
        Lt => "<",
        Ge => "≥",
        Le => "≤",
        In if g == Group::Choice => "is any of",
        NotIn => "is none of",
        In => "IN",
        NotInOrNull => "NOT IN … OR NULL",
        Between => "BETWEEN",
        IsNull => "IS NULL",
        NotNull => "IS NOT NULL",
        Contains => "contains",
        NotContains => "does not contain",
        StartsWith => "starts with",
        NotStartsWith => "does not start with",
        EndsWith => "ends with",
        NotEndsWith => "does not end with",
        Like => "LIKE",
        NotLike => "NOT LIKE",
        Regexp => "matches RegExp",
        NotRegexp => "does not match RegExp",
        Empty => "is empty",
        NotEmpty => "is not empty",
        LenEq => "length =",
        LenGt => "length >",
        LenLt => "length <",
        Today => "today",
        Last => "in the last ...",
        ThisWeek => "this week",
        ThisMonth => "this month",
        AfterCol => "is after column ...",
        SetContains => "SET contains",
        JsonPathEq => "JSON path =",
        JsonContains => "JSON contains",
        JsonHasPath => "JSON has path",
    }
}

/// The few menu entries drawn in the keyword color, as in the design.
pub fn is_keyword(op: Op) -> bool {
    use Op::*;
    matches!(op, In | NotInOrNull | Between | IsNull | NotNull | Like | NotLike)
}

fn is_number(s: &str) -> bool {
    let s = s.trim();
    s.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '.') && s.chars().all(|c| c.is_ascii_digit() || "-+.eE".contains(c)) && s.parse::<f64>().is_ok_and(|f| f.is_finite())
}

fn is_hex(s: &str) -> bool {
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit())
}

/// A value as SQL text: number columns get a bare number when it parses, everything else a quoted string.
fn val(kind: Kind, s: &str, bs: bool) -> String {
    let s = s.trim();
    if group_of(kind) == Group::Number && is_number(s) { s.to_string() } else { lit(s, bs) }
}

fn hex(s: &str, bs: bool) -> String {
    let s = s.trim();
    if is_hex(s) { format!("0x{}", s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s).to_ascii_uppercase()) } else { lit(s, bs) }
}

/// LIKE patterns treat % and _ as wildcards, so a search for them literally needs backslashes.
pub(super) fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn unlike(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(n) = it.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn unit_sql(u: &str) -> &'static str {
    match u {
        "hours" => "HOUR",
        "weeks" => "WEEK",
        "months" => "MONTH",
        _ => "DAY",
    }
}

/// The SQL of one rule.
pub fn rule_sql(r: &Rule, d: Dialect, bs: bool) -> String {
    use Op::*;
    let c = d.quote_ident(&r.col);
    let v = |i: usize| r.values.get(i).map(|s| s.as_str()).unwrap_or("");
    let list = || r.values.iter().filter(|s| !s.trim().is_empty()).map(|s| val(r.kind, s, bs)).collect::<Vec<_>>().join(", ");
    let other = group_of(r.kind) == Group::Other;
    let len = if group_of(r.kind) == Group::Text { "CHAR_LENGTH" } else { "LENGTH" };
    let n = || if is_number(v(0)) { v(0).trim().to_string() } else { "0".into() };
    match r.op {
        Eq if other => format!("{c} = {}", hex(v(0), bs)),
        Ne if other => format!("{c} <> {}", hex(v(0), bs)),
        Eq => format!("{c} = {}", val(r.kind, v(0), bs)),
        Ne => format!("{c} <> {}", val(r.kind, v(0), bs)),
        Gt => format!("{c} > {}", val(r.kind, v(0), bs)),
        Lt => format!("{c} < {}", val(r.kind, v(0), bs)),
        Ge => format!("{c} >= {}", val(r.kind, v(0), bs)),
        Le => format!("{c} <= {}", val(r.kind, v(0), bs)),
        In => format!("{c} IN ({})", list()),
        NotIn => format!("{c} NOT IN ({})", list()),
        NotInOrNull => format!("({c} NOT IN ({}) OR {c} IS NULL)", list()),
        Between => format!("{c} BETWEEN {} AND {}", val(r.kind, v(0), bs), val(r.kind, v(1), bs)),
        IsNull => format!("{c} IS NULL"),
        NotNull => format!("{c} IS NOT NULL"),
        Contains => format!("{c} LIKE {}", lit(&format!("%{}%", like_escape(v(0))), bs)),
        NotContains => format!("{c} NOT LIKE {}", lit(&format!("%{}%", like_escape(v(0))), bs)),
        StartsWith => format!("{c} LIKE {}", lit(&format!("{}%", like_escape(v(0))), bs)),
        NotStartsWith => format!("{c} NOT LIKE {}", lit(&format!("{}%", like_escape(v(0))), bs)),
        EndsWith => format!("{c} LIKE {}", lit(&format!("%{}", like_escape(v(0))), bs)),
        NotEndsWith => format!("{c} NOT LIKE {}", lit(&format!("%{}", like_escape(v(0))), bs)),
        Like => format!("{c} LIKE {}", lit(v(0), bs)),
        NotLike => format!("{c} NOT LIKE {}", lit(v(0), bs)),
        Regexp => format!("{c} REGEXP {}", lit(v(0), bs)),
        NotRegexp => format!("{c} NOT REGEXP {}", lit(v(0), bs)),
        Empty => format!("{c} = ''"),
        NotEmpty => format!("{c} <> ''"),
        LenEq => format!("{len}({c}) = {}", n()),
        LenGt => format!("{len}({c}) > {}", n()),
        LenLt => format!("{len}({c}) < {}", n()),
        Today => format!("DATE({c}) = CURDATE()"),
        Last => format!("{c} >= NOW() - INTERVAL {} {}", n(), unit_sql(v(1))),
        ThisWeek => format!("YEARWEEK({c}, 1) = YEARWEEK(CURDATE(), 1)"),
        ThisMonth => format!("(YEAR({c}) = YEAR(CURDATE()) AND MONTH({c}) = MONTH(CURDATE()))"),
        AfterCol => format!("{c} > {}", d.quote_ident(v(0))),
        SetContains => format!("FIND_IN_SET({}, {c}) > 0", lit(v(0), bs)),
        JsonPathEq => format!("JSON_UNQUOTE(JSON_EXTRACT({c}, {})) = {}", lit(v(0), bs), lit(v(1), bs)),
        JsonContains => format!("JSON_CONTAINS({c}, {})", lit(v(0), bs)),
        JsonHasPath => format!("JSON_CONTAINS_PATH({c}, 'one', {})", lit(v(0), bs)),
    }
}

/// The WHERE text of a rule list: complete rules only, joined with AND (or OR when `any`).
pub fn to_sql(rules: &[Rule], any: bool, d: Dialect, bs: bool) -> String {
    rules.iter().filter(|r| r.complete()).map(|r| rule_sql(r, d, bs)).collect::<Vec<_>>().join(if any { " OR " } else { " AND " })
}

#[derive(Clone, PartialEq, Debug)]
enum T {
    Id(String),
    Word(String),
    Str(String),
    Num(String),
    P(char),
    Cmp(&'static str),
}

/// Splits text into the few token kinds the rule SQL uses. Bare words that name a column become
/// identifiers, so `status` and `` `status` `` read the same.
fn lex(text: &str, cols: &[(String, Kind)], d: Dialect) -> Option<Vec<T>> {
    // Backticks hold names on MySQL, double quotes on PostgreSQL. The other mark is not a name there.
    let quote = if d == Dialect::Postgres { '"' } else { '`' };
    let ch: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < ch.len() {
        let c = ch[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == quote {
            let mut s = String::new();
            i += 1;
            loop {
                match ch.get(i)? {
                    q if *q == quote && ch.get(i + 1) == Some(&quote) => {
                        s.push(quote);
                        i += 2;
                    }
                    q if *q == quote => break,
                    c => {
                        s.push(*c);
                        i += 1;
                    }
                }
            }
            i += 1;
            out.push(T::Id(s));
        } else if c == '\'' {
            let mut s = String::new();
            i += 1;
            loop {
                match *ch.get(i)? {
                    '\'' if ch.get(i + 1) == Some(&'\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    '\'' => break,
                    '\\' => {
                        s.push('\\');
                        i += 1;
                        if let Some(n) = ch.get(i) {
                            s.push(*n);
                            i += 1;
                        }
                    }
                    c => {
                        s.push(c);
                        i += 1;
                    }
                }
            }
            i += 1;
            out.push(T::Str(unslash(&s)));
        } else if c.is_ascii_digit() || (c == '-' && ch.get(i + 1).is_some_and(|n| n.is_ascii_digit()) && !matches!(out.last(), Some(T::Id(_)) | Some(T::Num(_)) | Some(T::P(')')))) {
            let st = i;
            i += 1;
            while i < ch.len() && (ch[i].is_ascii_alphanumeric() || ch[i] == '.' || ((ch[i] == '-' || ch[i] == '+') && matches!(ch[i - 1], 'e' | 'E') && !ch[st..i].starts_with(&['0', 'x']))) {
                i += 1;
            }
            out.push(T::Num(ch[st..i].iter().collect()));
        } else if c.is_alphabetic() || c == '_' {
            let st = i;
            while i < ch.len() && (ch[i].is_alphanumeric() || ch[i] == '_') {
                i += 1;
            }
            let w: String = ch[st..i].iter().collect();
            let call = ch.get(i) == Some(&'(');
            match cols.iter().find(|(n, _)| n.eq_ignore_ascii_case(&w)) {
                Some((n, _)) if !call => out.push(T::Id(n.clone())),
                _ => out.push(T::Word(w.to_ascii_uppercase())),
            }
        } else if "(),-".contains(c) {
            out.push(T::P(c));
            i += 1;
        } else if "=<>".contains(c) {
            let two: String = ch[i..(i + 2).min(ch.len())].iter().collect();
            let (cmp, n) = match two.as_str() {
                "<>" => ("<>", 2),
                ">=" => (">=", 2),
                "<=" => ("<=", 2),
                _ => (match c { '=' => "=", '<' => "<", _ => ">" }, 1),
            };
            out.push(T::Cmp(cmp));
            i += n;
        } else {
            return None;
        }
    }
    Some(out)
}

/// Undoes the string escapes `lit` writes.
fn unslash(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' && it.peek() == Some(&'\\') {
            it.next();
        }
        out.push(c);
    }
    out
}

fn close_of(t: &[T], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, x) in t.iter().enumerate().skip(open) {
        match x {
            T::P('(') => depth += 1,
            T::P(')') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits at top-level AND or OR. BETWEEN owns the AND after its first value.
fn split(t: &[T]) -> Option<(Vec<&[T]>, bool)> {
    let (mut parts, mut start, mut depth, mut between) = (Vec::new(), 0, 0, false);
    let mut joiner: Option<bool> = None;
    for (i, x) in t.iter().enumerate() {
        match x {
            T::P('(') => depth += 1,
            T::P(')') => depth -= 1,
            T::Word(w) if depth == 0 && w == "BETWEEN" => between = true,
            T::Word(w) if depth == 0 && (w == "AND" || w == "OR") => {
                if w == "AND" && between {
                    between = false;
                    continue;
                }
                let any = w == "OR";
                if joiner.is_some_and(|j| j != any) {
                    return None;
                }
                joiner = Some(any);
                parts.push(&t[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&t[start..]);
    Some((parts, joiner.unwrap_or(false)))
}

fn word(t: &T, w: &str) -> bool {
    matches!(t, T::Word(x) if x == w)
}

fn value_of(t: &T) -> Option<String> {
    match t {
        T::Str(s) => Some(s.clone()),
        T::Num(s) => Some(s.clone()),
        _ => None,
    }
}

/// The values of `( a , b , c )` starting at `t[0]`, and how many tokens it took.
fn list_of(t: &[T]) -> Option<(Vec<String>, usize)> {
    if t.first() != Some(&T::P('(')) {
        return None;
    }
    let end = close_of(t, 0)?;
    let mut out = Vec::new();
    for (k, x) in t[1..end].iter().enumerate() {
        if k % 2 == 0 {
            out.push(value_of(x)?);
        } else if x != &T::P(',') {
            return None;
        }
    }
    Some((out, end + 1))
}

fn kind_of(cols: &[(String, Kind)], name: &str) -> Option<Kind> {
    cols.iter().find(|(n, _)| n == name).map(|(_, k)| *k)
}

fn rule_of(t: &[T], cols: &[(String, Kind)], d: Dialect) -> Option<Rule> {
    let mk = |col: &str, op: Op, values: Vec<String>| Some(Rule { col: col.into(), kind: kind_of(cols, col)?, op, values });
    // Wrapped forms: NOT IN ... OR NULL and this month.
    if t.first() == Some(&T::P('(')) && close_of(t, 0) == Some(t.len() - 1) {
        let inner = &t[1..t.len() - 1];
        if let [T::Id(c), n, i, T::P('('), ..] = inner {
            if word(n, "NOT") && word(i, "IN") {
                let (vals, used) = list_of(&inner[3..])?;
                let rest = &inner[3 + used..];
                if matches!(rest, [o, T::Id(c2), is, nu] if word(o, "OR") && c2 == c && word(is, "IS") && word(nu, "NULL")) {
                    return mk(c, Op::NotInOrNull, vals);
                }
            }
        }
        if let [y, T::P('('), T::Id(c), T::P(')'), ..] = inner {
            if word(y, "YEAR") && inner.len() == 23 {
                return mk(c, Op::ThisMonth, vec![]);
            }
        }
        return None;
    }
    // Function forms.
    match t {
        [d, T::P('('), T::Id(c), T::P(')'), T::Cmp("="), cd, T::P('('), T::P(')')] if word(d, "DATE") && word(cd, "CURDATE") => return mk(c, Op::Today, vec![]),
        [y, T::P('('), T::Id(c), T::P(','), T::Num(one), T::P(')'), T::Cmp("="), ..] if word(y, "YEARWEEK") && one == "1" => return mk(c, Op::ThisWeek, vec![]),
        [l, T::P('('), T::Id(c), T::P(')'), T::Cmp(o), T::Num(n)] if word(l, "CHAR_LENGTH") || word(l, "LENGTH") => {
            let op = match *o { "=" => Op::LenEq, ">" => Op::LenGt, "<" => Op::LenLt, _ => return None };
            return mk(c, op, vec![n.clone()]);
        }
        [f, T::P('('), T::Str(v), T::P(','), T::Id(c), T::P(')'), T::Cmp(">"), T::Num(z)] if word(f, "FIND_IN_SET") && z == "0" => return mk(c, Op::SetContains, vec![v.clone()]),
        [u, T::P('('), e, T::P('('), T::Id(c), T::P(','), T::Str(p), T::P(')'), T::P(')'), T::Cmp("="), T::Str(v)] if word(u, "JSON_UNQUOTE") && word(e, "JSON_EXTRACT") => return mk(c, Op::JsonPathEq, vec![p.clone(), v.clone()]),
        [f, T::P('('), T::Id(c), T::P(','), T::Str(v), T::P(')')] if word(f, "JSON_CONTAINS") => return mk(c, Op::JsonContains, vec![v.clone()]),
        [f, T::P('('), T::Id(c), T::P(','), T::Str(one), T::P(','), T::Str(p), T::P(')')] if word(f, "JSON_CONTAINS_PATH") && one == "one" => return mk(c, Op::JsonHasPath, vec![p.clone()]),
        _ => {}
    }
    let (T::Id(c), rest) = (t.first()?, &t[1..]) else { return None };
    let kind = kind_of(cols, c)?;
    let like = |s: &str, neg: bool| -> Option<Rule> {
        // A final % is a wildcard only after an even number of backslashes.
        let slashes = s.trim_end_matches('%').chars().rev().take_while(|c| *c == '\\').count();
        let (lead, trail) = (s.starts_with('%'), s.ends_with('%') && s.len() > 1 && slashes % 2 == 0);
        let core = s.strip_prefix('%').filter(|_| lead).unwrap_or(s);
        let core = if trail { &core[..core.len() - 1] } else { core };
        let plain = |x: &str| unlike(x);
        let (op, v) = match (lead, trail, neg) {
            (true, true, false) => (Op::Contains, plain(core)),
            (true, true, true) => (Op::NotContains, plain(core)),
            (false, true, false) => (Op::StartsWith, plain(core)),
            (false, true, true) => (Op::NotStartsWith, plain(core)),
            (true, false, false) => (Op::EndsWith, plain(core)),
            (true, false, true) => (Op::NotEndsWith, plain(core)),
            _ => return None,
        };
        // A pattern that is not what the structured form would write stays a plain LIKE.
        let probe = Rule { col: c.clone(), kind, op, values: vec![v.clone()] };
        if rule_sql(&probe, d, true).ends_with(&lit(s, true)) { Some(probe) } else { None }
    };
    match rest {
        [i, nu] if word(i, "IS") && word(nu, "NULL") => mk(c, Op::IsNull, vec![]),
        [i, n, nu] if word(i, "IS") && word(n, "NOT") && word(nu, "NULL") => mk(c, Op::NotNull, vec![]),
        [b, x, a, y] if word(b, "BETWEEN") && word(a, "AND") => mk(c, Op::Between, vec![value_of(x)?, value_of(y)?]),
        [i, ..] if word(i, "IN") => mk(c, Op::In, list_of(&rest[1..])?.0),
        [n, i, ..] if word(n, "NOT") && word(i, "IN") => mk(c, Op::NotIn, list_of(&rest[2..])?.0),
        [l, T::Str(s)] if word(l, "LIKE") => like(s, false).or_else(|| mk(c, Op::Like, vec![s.clone()])),
        [n, l, T::Str(s)] if word(n, "NOT") && word(l, "LIKE") => like(s, true).or_else(|| mk(c, Op::NotLike, vec![s.clone()])),
        [r, T::Str(s)] if word(r, "REGEXP") => mk(c, Op::Regexp, vec![s.clone()]),
        [n, r, T::Str(s)] if word(n, "NOT") && word(r, "REGEXP") => mk(c, Op::NotRegexp, vec![s.clone()]),
        [T::Cmp(">="), n, T::P('('), T::P(')'), T::P('-'), iv, T::Num(k), u] if word(n, "NOW") && word(iv, "INTERVAL") => {
            let unit = match u { T::Word(w) if w == "HOUR" => "hours", T::Word(w) if w == "DAY" => "days", T::Word(w) if w == "WEEK" => "weeks", T::Word(w) if w == "MONTH" => "months", _ => return None };
            mk(c, Op::Last, vec![k.clone(), unit.into()])
        }
        [T::Cmp(">"), T::Id(o)] => mk(c, Op::AfterCol, vec![o.clone()]),
        [T::Cmp("="), T::Str(s)] if s.is_empty() && group_of(kind) == Group::Text => mk(c, Op::Empty, vec![]),
        [T::Cmp("<>"), T::Str(s)] if s.is_empty() && group_of(kind) == Group::Text => mk(c, Op::NotEmpty, vec![]),
        [T::Cmp(o), v] => {
            let op = match *o { "=" => Op::Eq, "<>" => Op::Ne, ">" => Op::Gt, "<" => Op::Lt, ">=" => Op::Ge, _ => Op::Le };
            mk(c, op, vec![value_of(v)?])
        }
        _ => None,
    }
}

/// Reads text back into rules when it is a flat AND or OR list of what `to_sql` writes. Returns
/// the rules and whether they are joined with OR. Anything else (subqueries, mixed AND/OR, other
/// functions) gives None, which keeps the bar in SQL mode. The result is checked by writing it
/// out again, so a successful parse never changes the query.
pub fn parse(text: &str, cols: &[(String, Kind)], d: Dialect, bs: bool) -> Option<(Vec<Rule>, bool)> {
    // The reader below decodes backslash escapes, which this server does not have, so such text stays SQL.
    if !bs && text.contains('\\') {
        return None;
    }
    let toks = lex(text, cols, d)?;
    if toks.is_empty() {
        return Some((Vec::new(), false));
    }
    let (parts, any) = split(&toks)?;
    let rules: Vec<Rule> = parts.iter().map(|p| rule_of(p, cols, d)).collect::<Option<_>>()?;
    let any = any && rules.len() > 1;
    if !rules.iter().all(|r| ops_for(r.kind).contains(&r.op) && r.complete()) {
        return None;
    }
    (lex(&to_sql(&rules, any, d, true), cols, d)? == toks).then_some((rules, any))
}

/// How a date column is grouped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grain {
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Grain {
    pub const ALL: [(Grain, &'static str); 5] = [(Grain::Hour, "hour"), (Grain::Day, "day"), (Grain::Week, "week"), (Grain::Month, "month"), (Grain::Year, "year")];

    pub fn name(self) -> &'static str {
        Grain::ALL.iter().find(|(g, _)| *g == self).map(|(_, n)| *n).unwrap_or("day")
    }

    /// The grouping expression, such as `DATE(created_at)` for day.
    pub fn expr(self, col: &str, d: Dialect) -> String {
        let c = d.quote_ident(col);
        match self {
            Grain::Hour => format!("DATE_FORMAT({c}, '%Y-%m-%d %H:00')"),
            Grain::Day => format!("DATE({c})"),
            Grain::Week => format!("YEARWEEK({c}, 3)"),
            Grain::Month => format!("DATE_FORMAT({c}, '%Y-%m')"),
            Grain::Year => format!("YEAR({c})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<(String, Kind)> {
        [("id", Kind::Int), ("total", Kind::Decimal), ("name", Kind::Text), ("created_at", Kind::DateTime), ("status", Kind::Enum), ("tags", Kind::Set), ("data", Kind::Json), ("blob", Kind::Binary), ("seen_at", Kind::DateTime)]
            .iter()
            .map(|(n, k)| (n.to_string(), *k))
            .collect()
    }

    fn vals(op: Op) -> Vec<String> {
        match op {
            Op::Between => vec!["a".into(), "b".into()],
            Op::In | Op::NotIn | Op::NotInOrNull => vec!["x".into(), "it's".into(), "back\\slash".into()],
            Op::Last => vec!["3".into(), "weeks".into()],
            Op::JsonPathEq => vec!["$.a.b".into(), "v".into()],
            Op::AfterCol => vec!["seen_at".into()],
            Op::LenEq | Op::LenGt | Op::LenLt => vec!["5".into()],
            Op::Contains | Op::NotContains | Op::StartsWith | Op::NotStartsWith | Op::EndsWith | Op::NotEndsWith => vec!["50%_off\\".into()],
            Op::SetContains => vec!["a".into()],
            _ => default_values(op).into_iter().map(|_| "ab'c".to_string()).collect(),
        }
    }

    #[test]
    fn every_op_round_trips() {
        for (col, kind) in cols() {
            for &op in ops_for(kind) {
                let mut v = vals(op);
                if group_of(kind) == Group::Number && matches!(op, Op::Eq | Op::Ne | Op::Gt | Op::Lt | Op::Ge | Op::Le) {
                    v = vec!["50.5".into()];
                }
                if group_of(kind) == Group::Other && matches!(op, Op::Eq | Op::Ne) {
                    v = vec!["0x5A3F9C".into()];
                }
                if group_of(kind) == Group::Number && op == Op::Between {
                    v = vec!["50".into(), "200".into()];
                }
                if group_of(kind) == Group::Number && matches!(op, Op::In | Op::NotInOrNull) {
                    v = vec!["1".into(), "2".into()];
                }
                if matches!(op, Op::AfterCol) && col != "created_at" {
                    continue;
                }
                let r = Rule { col: col.clone(), kind, op, values: v };
                assert!(r.complete(), "{op:?}");
                let sql = to_sql(&[r.clone()], false, Dialect::MySql, true);
                let (back, any) = parse(&sql, &cols(), Dialect::MySql, true).unwrap_or_else(|| panic!("no parse for {op:?} on {col}: {sql}"));
                assert!(!any);
                assert_eq!(back, vec![r], "{sql}");
            }
        }
    }

    #[test]
    fn postgres_rules_round_trip_through_double_quoted_names() {
        let d = Dialect::Postgres;
        let a = Rule { col: "status".into(), kind: Kind::Enum, op: Op::In, values: vec!["paid".into(), "shipped".into()] };
        let b = Rule { col: "to\"tal".into(), kind: Kind::Decimal, op: Op::Gt, values: vec!["50".into()] };
        let sql = to_sql(&[a.clone(), b.clone()], false, d, false);
        assert_eq!(sql, "\"status\" IN ('paid', 'shipped') AND \"to\"\"tal\" > 50");
        assert_eq!(parse(&sql, &[("status".into(), Kind::Enum), ("to\"tal".into(), Kind::Decimal)], d, false), Some((vec![a, b], false)));
        // A backtick name is not a name on PostgreSQL, and a double quoted one is not on MySQL.
        assert!(parse("`status` = 'paid'", &cols(), d, false).is_none());
        assert!(parse("\"status\" = 'paid'", &cols(), Dialect::MySql, true).is_none());
    }

    #[test]
    fn lists_join_with_and_or_or_and_reject_mixes() {
        let a = Rule { col: "status".into(), kind: Kind::Enum, op: Op::In, values: vec!["paid".into(), "shipped".into()] };
        let b = Rule { col: "total".into(), kind: Kind::Decimal, op: Op::Gt, values: vec!["50".into()] };
        let and = to_sql(&[a.clone(), b.clone()], false, Dialect::MySql, true);
        assert_eq!(and, "`status` IN ('paid', 'shipped') AND `total` > 50");
        assert_eq!(parse(&and, &cols(), Dialect::MySql, true), Some((vec![a.clone(), b.clone()], false)));
        let or = to_sql(&[a.clone(), b.clone()], true, Dialect::MySql, true);
        assert_eq!(parse(&or, &cols(), Dialect::MySql, true), Some((vec![a, b], true)));
        assert!(parse("total > 50 AND total < 5 OR id = 1", &cols(), Dialect::MySql, true).is_none());
        assert!(parse("total > 50 AND id IN (SELECT 1)", &cols(), Dialect::MySql, true).is_none());
        assert!(parse("LOWER(name) = 'x'", &cols(), Dialect::MySql, true).is_none());
        assert_eq!(parse("", &cols(), Dialect::MySql, true), Some((vec![], false)));
        // Bare column names and lower case keywords are fine.
        assert_eq!(parse("status = 'paid' and total between 1 and 2", &cols(), Dialect::MySql, true).unwrap().0.len(), 2);
    }

    #[test]
    fn values_are_escaped_and_numbers_stay_bare_only_in_number_columns() {
        let t = Rule { col: "name".into(), kind: Kind::Text, op: Op::Eq, values: vec!["5".into()] };
        assert_eq!(rule_sql(&t, Dialect::MySql, true), "`name` = '5'");
        let n = Rule { col: "total".into(), kind: Kind::Decimal, op: Op::Eq, values: vec!["5 OR 1=1".into()] };
        assert_eq!(rule_sql(&n, Dialect::MySql, true), "`total` = '5 OR 1=1'");
        let q = Rule { col: "na`me".into(), kind: Kind::Text, op: Op::Eq, values: vec!["a'\\".into()] };
        assert_eq!(rule_sql(&q, Dialect::MySql, true), "`na``me` = 'a''\\\\'");
        // Without backslash escapes the backslash is a plain character, and text with one stays SQL.
        assert_eq!(rule_sql(&q, Dialect::MySql, false), "`na``me` = 'a''\\'");
        assert!(parse("name = 'a\\b'", &cols(), Dialect::MySql, false).is_none());
        // Without backslash escapes the backslash is a plain character, and text with one stays SQL.
        assert_eq!(rule_sql(&q, Dialect::MySql, false), "`na``me` = 'a''\\'");
        assert!(parse("name = 'a\\b'", &cols(), Dialect::MySql, false).is_none());
    }

    #[test]
    fn incomplete_rules_are_left_out() {
        let r = Rule::new("total", Kind::Decimal);
        assert!(!r.complete());
        assert_eq!(to_sql(&[r], false, Dialect::MySql, true), "");
    }

    #[test]
    fn grains() {
        assert_eq!(Grain::Day.expr("created_at", Dialect::MySql), "DATE(`created_at`)");
        assert_eq!(Grain::Week.expr("c", Dialect::MySql), "YEARWEEK(`c`, 3)");
    }
}
