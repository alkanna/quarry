//! Typed filter rules of the Content bar: the operators per column type (Sequel Ace's lists), the
//! SQL a rule list turns into, and the way back for text this module wrote itself.

use super::value::{lit, Kind};
use super::Dialect;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
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
    /// Differs from the value, NULL counting as a value of its own.
    Distinct,
    Yesterday,
    LastMonth,
    JsonPathNe,
    JsonPathGt,
    JsonPathLt,
    /// A top-level key of a JSON object. PostgreSQL only.
    JsonHasKey,
    /// The array holds every one of the values. PostgreSQL only, like the other array operators.
    ArrayHasAll,
    ArrayHasAny,
    ArrayLenEq,
    ArrayLenGt,
    ArrayLenLt,
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
        // The PostgreSQL kinds that are text as typed share the text operators until their own are designed.
        Kind::Text | Kind::Uuid | Kind::Array | Kind::Interval | Kind::Inet | Kind::Range => Group::Text,
        Kind::Date | Kind::DateTime => Group::Date,
        Kind::Enum | Kind::Set | Kind::Bool => Group::Choice,
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

/// One rule. `kind` decides how values are written (only number columns get bare numbers). It is not saved, because the
/// column's type is read from the table again when a saved rule comes back.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Rule {
    pub col: String,
    #[serde(skip, default)]
    pub kind: Kind,
    pub op: Op,
    pub values: Vec<String>,
    /// Joined to the rule before it by the other connector than the list's, which groups them in parentheses:
    /// `a AND (b OR c)` is a list joined by AND whose `c` joins `b`. One level deep, which covers what a bar of rules shows.
    #[serde(skip, default)]
    pub join: bool,
    /// Match case, for the text operators of a text column (see `has_case`). Elsewhere it stays `default_case`.
    #[serde(default)]
    pub case: bool,
}

impl Rule {
    pub fn new(col: &str, kind: Kind, d: Dialect) -> Rule {
        let op = ops_for(kind, d)[0];
        Rule { join: false, col: col.into(), kind, op, values: default_values(op), case: default_case(op, d) }
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

/// Whether the rule's `case` flag changes its SQL.
pub fn has_case(kind: Kind, op: Op) -> bool {
    use Op::*;
    kind == Kind::Text && matches!(op, Eq | Ne | Contains | NotContains | StartsWith | NotStartsWith | EndsWith | NotEndsWith | Regexp | NotRegexp)
}

/// What a new rule matches by: PostgreSQL's = compares case like its regex does, while its text search and MySQL's
/// default collations ignore it. Only = and its opposite on PostgreSQL start out matching case.
pub fn default_case(op: Op, d: Dialect) -> bool {
    d == Dialect::Postgres && matches!(op, Op::Eq | Op::Ne)
}

pub fn input(op: Op) -> Input {
    use Op::*;
    match op {
        IsNull | NotNull | Empty | NotEmpty | Today | Yesterday | ThisWeek | ThisMonth | LastMonth => Input::None,
        Between => Input::Two,
        In | NotIn | NotInOrNull | ArrayHasAll | ArrayHasAny => Input::List,
        Last => Input::Last,
        JsonPathEq | JsonPathNe | JsonPathGt | JsonPathLt => Input::Path,
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
pub fn ops_for(kind: Kind, d: Dialect) -> &'static [Op] {
    use Op::*;
    match group_of(kind) {
        Group::Number => &[Eq, Ne, Gt, Lt, Ge, Le, In, NotInOrNull, Between, Distinct, IsNull, NotNull],
        // Arrays exist on PostgreSQL only, and keep the text operators on their text form after their own.
        Group::Text if kind == Kind::Array => &[ArrayHasAny, ArrayHasAll, ArrayLenEq, ArrayLenGt, ArrayLenLt, Eq, Ne, Contains, NotContains, StartsWith, NotStartsWith, EndsWith, NotEndsWith, Like, NotLike, Regexp, NotRegexp, In, NotInOrNull, Distinct, Empty, NotEmpty, IsNull, NotNull, LenEq, LenGt, LenLt],
        Group::Text => &[Eq, Ne, Contains, NotContains, StartsWith, NotStartsWith, EndsWith, NotEndsWith, Like, NotLike, Regexp, NotRegexp, In, NotInOrNull, Distinct, Empty, NotEmpty, IsNull, NotNull, LenEq, LenGt, LenLt],
        Group::Date => &[Eq, Ne, Lt, Gt, Between, Today, Yesterday, Last, ThisWeek, ThisMonth, LastMonth, AfterCol, Distinct, IsNull, NotNull],
        Group::Choice => &[Eq, Ne, In, NotIn, SetContains, Distinct, IsNull, NotNull],
        Group::Other if kind == Kind::Json && d == Dialect::Postgres => &[Eq, Ne, LenEq, JsonPathEq, JsonPathNe, JsonPathGt, JsonPathLt, JsonContains, JsonHasPath, JsonHasKey, IsNull, NotNull],
        Group::Other if kind == Kind::Json => &[Eq, Ne, LenEq, JsonPathEq, JsonPathNe, JsonPathGt, JsonPathLt, JsonContains, JsonHasPath, IsNull, NotNull],
        // Binary, bit and geometry values are not JSON documents, so the JSON operators would only fail.
        Group::Other => &[Eq, Ne, LenEq, IsNull, NotNull],
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
        Distinct => "is distinct from",
        Yesterday => "yesterday",
        LastMonth => "last month",
        JsonPathNe => "JSON path ≠",
        JsonPathGt => "JSON path >",
        JsonPathLt => "JSON path <",
        JsonHasKey => "has key",
        ArrayHasAll => "has all of",
        ArrayHasAny => "has any of",
        ArrayLenEq => "item count =",
        ArrayLenGt => "item count >",
        ArrayLenLt => "item count <",
    }
}

/// The SQL an operator sends, for the right column of the operator menu: the rule written for a column `x` with
/// placeholder values.
pub fn op_sql_hint(op: Op, kind: Kind, d: Dialect, case: bool) -> String {
    let values = match input(op) {
        Input::None => vec![],
        Input::One => vec!["…".into()],
        Input::Two => vec!["…".into(), "…".into()],
        Input::List => vec!["a".into(), "b".into()],
        Input::Last => vec!["7".into(), "days".into()],
        Input::Path => vec!["$.k".into(), "…".into()],
        Input::Column => vec!["y".into()],
    };
    let r = Rule { join: false, col: "x".into(), kind, op, values, case };
    rule_sql(&r, d, true).replace(&d.quote_ident("x"), "x").replace(&d.quote_ident("y"), "y")
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
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// `like_escape` for a pattern written with `ESCAPE '!'`, which every server and sql_mode reads the same way.
pub(super) fn like_bang(s: &str) -> String {
    s.replace('!', "!!").replace('%', "!%").replace('_', "!_")
}

/// A pattern from `like_escape` as a literal. Under NO_BACKSLASH_ESCAPES MySQL's LIKE has no escape character at
/// all, so one that holds a backslash names it. Plain patterns stay bare so the bar can read them back.
fn like_lit(pat: &str, d: Dialect, bs: bool) -> String {
    if !bs && d != Dialect::Postgres && pat.contains('\\') { format!("{} ESCAPE '\\'", lit(pat, bs)) } else { lit(pat, bs) }
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

/// A value compared as a number when it reads as one, else as text the server converts.
fn num_or_lit(s: &str, bs: bool) -> String {
    if is_number(s) { s.trim().to_string() } else { lit(s, bs) }
}

/// The SQL of one rule.
pub fn rule_sql(r: &Rule, d: Dialect, bs: bool) -> String {
    use Op::*;
    let mut c = d.quote_ident(&r.col);
    // PostgreSQL's uuid, inet, range, interval and array types take no LIKE or text length, so they are read as text.
    if d == Dialect::Postgres && group_of(r.kind) == Group::Text && r.kind != Kind::Text {
        c = format!("{c}::text");
    }
    let v = |i: usize| r.values.get(i).map(|s| s.as_str()).unwrap_or("");
    let list = || r.values.iter().filter(|s| !s.trim().is_empty()).map(|s| val(r.kind, s, bs)).collect::<Vec<_>>().join(", ");
    let other = group_of(r.kind) == Group::Other;
    let len = if group_of(r.kind) == Group::Text { "CHAR_LENGTH" } else { "LENGTH" };
    let n = || if is_number(v(0)) { v(0).trim().to_string() } else { "0".into() };
    if d == Dialect::Postgres && let Some(sql) = pg_rule_sql(r, &c, n) {
        return sql;
    }
    // MySQL compares by the column's collation, which ignores case by default; its bytes match case.
    let cased = has_case(r.kind, r.op) && r.case && d != Dialect::Postgres;
    let cb = if cased { format!("CAST({c} AS BINARY)") } else { c.clone() };
    let like = |pre: &str, suf: &str| like_lit(&format!("{pre}{}{suf}", like_escape(v(0))), d, bs);
    // An inline flag, because MySQL 8's ICU and MariaDB's PCRE both read it while REGEXP has no case argument.
    let rx = || lit(&if cased { format!("(?-i){}", v(0)) } else { v(0).to_string() }, bs);
    match r.op {
        Eq if other => format!("{c} = {}", hex(v(0), bs)),
        Ne if other => format!("{c} <> {}", hex(v(0), bs)),
        Eq => format!("{cb} = {}", val(r.kind, v(0), bs)),
        Ne => format!("{cb} <> {}", val(r.kind, v(0), bs)),
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
        Contains => format!("{cb} LIKE {}", like("%", "%")),
        NotContains => format!("{cb} NOT LIKE {}", like("%", "%")),
        StartsWith => format!("{cb} LIKE {}", like("", "%")),
        NotStartsWith => format!("{cb} NOT LIKE {}", like("", "%")),
        EndsWith => format!("{cb} LIKE {}", like("%", "")),
        NotEndsWith => format!("{cb} NOT LIKE {}", like("%", "")),
        Like => format!("{c} LIKE {}", lit(v(0), bs)),
        NotLike => format!("{c} NOT LIKE {}", lit(v(0), bs)),
        Regexp => format!("{c} REGEXP {}", rx()),
        NotRegexp => format!("{c} NOT REGEXP {}", rx()),
        Empty => format!("{c} = ''"),
        NotEmpty => format!("{c} <> ''"),
        LenEq => format!("{len}({c}) = {}", n()),
        LenGt => format!("{len}({c}) > {}", n()),
        LenLt => format!("{len}({c}) < {}", n()),
        Today => format!("DATE({c}) = CURDATE()"),
        Yesterday => format!("DATE({c}) = CURDATE() - INTERVAL 1 DAY"),
        Last => format!("{c} >= NOW() - INTERVAL {} {}", n(), unit_sql(v(1))),
        ThisWeek => format!("YEARWEEK({c}, 1) = YEARWEEK(CURDATE(), 1)"),
        ThisMonth => format!("(YEAR({c}) = YEAR(CURDATE()) AND MONTH({c}) = MONTH(CURDATE()))"),
        // One comparison, so the rule needs no parentheses of its own.
        LastMonth => format!("EXTRACT(YEAR_MONTH FROM {c}) = EXTRACT(YEAR_MONTH FROM CURDATE() - INTERVAL 1 MONTH)"),
        AfterCol => format!("{c} > {}", d.quote_ident(v(0))),
        SetContains => format!("FIND_IN_SET({}, {c}) > 0", lit(v(0), bs)),
        JsonPathEq => format!("JSON_UNQUOTE(JSON_EXTRACT({c}, {})) = {}", lit(v(0), bs), lit(v(1), bs)),
        JsonPathNe => format!("JSON_UNQUOTE(JSON_EXTRACT({c}, {})) <> {}", lit(v(0), bs), lit(v(1), bs)),
        // JSON values compare as numbers against a number.
        JsonPathGt => format!("JSON_EXTRACT({c}, {}) > {}", lit(v(0), bs), num_or_lit(v(1), bs)),
        JsonPathLt => format!("JSON_EXTRACT({c}, {}) < {}", lit(v(0), bs), num_or_lit(v(1), bs)),
        JsonContains => format!("JSON_CONTAINS({c}, {})", lit(v(0), bs)),
        JsonHasPath => format!("JSON_CONTAINS_PATH({c}, 'one', {})", lit(v(0), bs)),
        Distinct if d == Dialect::Postgres => format!("{c} IS DISTINCT FROM {}", val(r.kind, v(0), bs)),
        Distinct => format!("NOT ({c} <=> {})", val(r.kind, v(0), bs)),
        // The menu offers these on PostgreSQL only; MySQL gets the JSON array forms of the same question.
        JsonHasKey => format!("JSON_CONTAINS_PATH({c}, 'one', CONCAT('$.', JSON_QUOTE({})))", lit(v(0), bs)),
        ArrayHasAll => format!("JSON_CONTAINS({c}, JSON_ARRAY({}))", list()),
        ArrayHasAny => format!("JSON_OVERLAPS({c}, JSON_ARRAY({}))", list()),
        ArrayLenEq => format!("JSON_LENGTH({c}) = {}", n()),
        ArrayLenGt => format!("JSON_LENGTH({c}) > {}", n()),
        ArrayLenLt => format!("JSON_LENGTH({c}) < {}", n()),
    }
}

/// PostgreSQL's spelling of the operators whose MySQL form it does not have. Its jsonpath reads the same `$.a.b`.
fn pg_rule_sql(r: &Rule, c: &str, n: impl Fn() -> String) -> Option<String> {
    use Op::*;
    let v = |i: usize| super::value::pg_lit(r.values.get(i).map(|s| s.as_str()).unwrap_or(""));
    // MySQL's 0x literal and LENGTH of a JSON value are not PostgreSQL; bytea takes '\x..' and the rest compare as text.
    let typed = || match r.values.first().map(|s| s.trim()) {
        Some(x) if r.kind == Kind::Binary && is_hex(x) => super::value::pg_lit(&format!("\\x{}", x.trim_start_matches("0x").trim_start_matches("0X"))),
        _ => v(0),
    };
    // `c` already ends in ::text for uuid, inet and the other text-like kinds (see `rule_sql`).
    let txt = if c.ends_with("::text") { c.to_string() } else { format!("{c}::text") };
    let raw = c.strip_suffix("::text").unwrap_or(c);
    let side = || if r.kind == Kind::Binary { c.to_string() } else { txt.clone() };
    let other = group_of(r.kind) == Group::Other;
    // Text search ignores case and regex matches it unless the rule's own flag says otherwise, as they always did
    // before the flag; = matches case (see `default_case`).
    let ci = has_case(r.kind, r.op) && !r.case;
    let like_kw = if !has_case(r.kind, r.op) || ci { "ILIKE" } else { "LIKE" };
    let rx = if ci { "~*" } else { "~" };
    // The pattern is escaped with backslashes, the default escape of both LIKE and ILIKE.
    let like = |pre: &str, suf: &str| like_lit(&format!("{pre}{}{suf}", like_escape(r.values.first().map_or("", |s| s.as_str()))), Dialect::Postgres, false);
    let items = || r.values.iter().filter(|s| !s.trim().is_empty()).map(|s| super::value::pg_lit(s)).collect::<Vec<_>>().join(", ");
    let path = || format!("jsonb_path_query_first({c}::jsonb, {}) #>> '{{}}'", v(0));
    let num = || r.values.get(1).filter(|s| is_number(s)).map_or_else(|| v(1), |s| s.trim().to_string());
    Some(match r.op {
        Contains => format!("{c} {like_kw} {}", like("%", "%")),
        NotContains => format!("{c} NOT {like_kw} {}", like("%", "%")),
        StartsWith => format!("{c} {like_kw} {}", like("", "%")),
        NotStartsWith => format!("{c} NOT {like_kw} {}", like("", "%")),
        EndsWith => format!("{c} {like_kw} {}", like("%", "")),
        NotEndsWith => format!("{c} NOT {like_kw} {}", like("%", "")),
        Eq if ci => format!("lower({c}) = lower({})", v(0)),
        Ne if ci => format!("lower({c}) <> lower({})", v(0)),
        Eq if r.kind == Kind::Json => format!("{c} = {}::jsonb", v(0)),
        Ne if r.kind == Kind::Json => format!("{c} <> {}::jsonb", v(0)),
        Eq if other => format!("{} = {}", side(), typed()),
        Ne if other => format!("{} <> {}", side(), typed()),
        LenEq if other => format!("octet_length({}) = {}", side(), n()),
        LenGt if other => format!("octet_length({}) > {}", side(), n()),
        LenLt if other => format!("octet_length({}) < {}", side(), n()),
        Regexp => format!("{txt} {rx} {}", v(0)),
        NotRegexp => format!("{txt} !{rx} {}", v(0)),
        Today => format!("{c}::date = CURRENT_DATE"),
        Yesterday => format!("{c}::date = CURRENT_DATE - 1"),
        Last => format!("{c} >= NOW() - INTERVAL '{} {}'", n(), unit_sql(r.values.get(1).map_or("", |s| s.as_str()))),
        ThisWeek => format!("date_trunc('week', {c}) = date_trunc('week', CURRENT_DATE)"),
        ThisMonth => format!("date_trunc('month', {c}) = date_trunc('month', CURRENT_DATE)"),
        LastMonth => format!("date_trunc('month', {c}) = date_trunc('month', CURRENT_DATE) - INTERVAL '1 month'"),
        SetContains => format!("{} = ANY(string_to_array({txt}, ','))", v(0)),
        JsonPathEq => format!("{} = {}", path(), v(1)),
        JsonPathNe => format!("{} <> {}", path(), v(1)),
        JsonPathGt => format!("({})::numeric > {}", path(), num()),
        JsonPathLt => format!("({})::numeric < {}", path(), num()),
        JsonContains => format!("{c}::jsonb @> {}::jsonb", v(0)),
        JsonHasPath => format!("jsonb_path_exists({c}::jsonb, {})", v(0)),
        JsonHasKey => format!("{c}::jsonb ? {}", v(0)),
        // The column is cast rather than the values, so int[], uuid[] and text[] all take the same text items.
        ArrayHasAll => format!("{raw}::text[] @> ARRAY[{}]", items()),
        ArrayHasAny => format!("{raw}::text[] && ARRAY[{}]", items()),
        ArrayLenEq => format!("cardinality({raw}) = {}", n()),
        ArrayLenGt => format!("cardinality({raw}) > {}", n()),
        ArrayLenLt => format!("cardinality({raw}) < {}", n()),
        _ => return None,
    })
}

/// The Query help window's text for a table's Content bar. The rule lines are written by `rule_sql`, so they show what
/// this dialect is sent and cannot drift from it.
pub fn filter_help(d: Dialect) -> String {
    const COL: usize = 31;
    let row = |a: &str, b: &str| format!("  {a:<COL$}{b}\n");
    let mut s = String::from("The filter bar\n");
    s += &row("Rules / SQL", "build the WHERE from rules, or type any SQL condition");
    s += &row("Enter", "run the filter");
    s += &row("ORDER BY", "any SQL sort; a column header click sorts too, Shift adds a key");
    s += &row("Up / Down, Enter or Tab, Esc", "move in the completion list, insert, close");
    s += "\nRules: pick a column, then an operator. What some of them send\n";
    let base: [(Op, Kind, &[&str]); 12] = [
        (Op::Contains, Kind::Text, &["paid"]),
        (Op::StartsWith, Kind::Text, &["A"]),
        (Op::Regexp, Kind::Text, &["^[0-9]+$"]),
        (Op::In, Kind::Text, &["paid", "sent"]),
        (Op::NotInOrNull, Kind::Text, &["void"]),
        (Op::Distinct, Kind::Text, &["paid"]),
        (Op::Last, Kind::DateTime, &["7", "days"]),
        (Op::ThisMonth, Kind::DateTime, &[]),
        (Op::LastMonth, Kind::DateTime, &[]),
        (Op::JsonPathEq, Kind::Json, &["$.a.b", "x"]),
        (Op::JsonPathGt, Kind::Json, &["$.n", "5"]),
        (Op::JsonContains, Kind::Json, &["{\"a\": 1}"]),
    ];
    let pg: [(Op, Kind, &[&str]); 3] = [(Op::JsonHasKey, Kind::Json, &["a"]), (Op::ArrayHasAny, Kind::Array, &["a", "b"]), (Op::ArrayLenGt, Kind::Array, &["2"])];
    for (op, kind, vals) in base.into_iter().chain(pg.into_iter().filter(|_| d == Dialect::Postgres)) {
        let r = Rule { join: false, col: "name".into(), kind, op, values: vals.iter().map(|v| v.to_string()).collect(), case: default_case(op, d) };
        s += &row(label(op, kind), &rule_sql(&r, d, false));
    }
    let cased = Rule { join: false, col: "name".into(), kind: Kind::Text, op: Op::Contains, values: vec!["paid".into()], case: true };
    s += &row("Aa (match case)", &rule_sql(&cased, d, false));
    if d == Dialect::Postgres {
        s += &row("", "Text search and regex ignore case until Aa is on, like MySQL's default collations; = matches it");
    }
    s += "\nShortcuts\n";
    s += &row("Cmd+F", "focus the WHERE box");
    s += &row("Cmd+E", "explain the filter's query");
    s += &row("Up / Down", "move between rows");
    s += &row("PageUp / PageDown", "previous and next page");
    s += &row("Cmd+S", "save the filter to the script library");
    s
}

/// The WHERE text of a rule list: complete rules only, joined with AND (or OR when `any`), and a rule that `join`s the
/// one before it grouped with it in parentheses under the other connector.
pub fn to_sql(rules: &[Rule], any: bool, d: Dialect, bs: bool) -> String {
    let (outer, inner) = if any { (" OR ", " AND ") } else { (" AND ", " OR ") };
    let mut groups: Vec<Vec<String>> = Vec::new();
    for r in rules.iter().filter(|r| r.complete()) {
        let s = rule_sql(r, d, bs);
        match groups.last_mut() {
            Some(g) if r.join => g.push(s),
            _ => groups.push(vec![s]),
        }
    }
    // One group is a plain list in the other connector, with nothing to group it against.
    if let [g] = groups.as_slice() {
        return g.join(inner);
    }
    groups.iter().map(|g| if g.len() > 1 { format!("({})", g.join(inner)) } else { g[0].clone() }).collect::<Vec<_>>().join(outer)
}

/// Rules all joined by the other connector are a plain list in it: the list's connector flips and the joins clear, so the
/// bar shows what `parse` would read back. The first rule never joins.
pub fn normalize(rules: &mut [Rule], any: &mut bool) {
    if rules.len() > 1 && rules[1..].iter().all(|r| r.join) {
        *any = !*any;
        rules.iter_mut().for_each(|r| r.join = false);
    }
    if let Some(r) = rules.first_mut() {
        r.join = false;
    }
}

/// Removes rule `i`. A rule that started a group hands the start to the next one, which would otherwise join the group before.
pub fn remove(rules: &mut Vec<Rule>, any: &mut bool, i: usize) {
    if i >= rules.len() {
        return;
    }
    let r = rules.remove(i);
    if !r.join && let Some(next) = rules.get_mut(i) {
        next.join = false;
    }
    normalize(rules, any);
}

#[derive(Clone, PartialEq, Debug)]
enum T {
    Id(String),
    Word(String),
    Str(String),
    Num(String),
    P(char),
    Cmp(&'static str),
    /// PostgreSQL's `::` cast.
    Cast,
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
        } else if "(),-[]".contains(c) {
            out.push(T::P(c));
            i += 1;
        } else if let Some((t, n)) = [("::", T::Cast), ("~*", T::Cmp("~*")), ("~", T::Cmp("~")), ("!~*", T::Cmp("!~*")), ("!~", T::Cmp("!~")), ("@>", T::Cmp("@>")), ("#>>", T::Cmp("#>>")), ("&&", T::Cmp("&&")), ("?", T::Cmp("?"))].into_iter().find(|(p, _)| ch[i..].starts_with(&p.chars().collect::<Vec<_>>())).map(|(p, t)| (t, p.len())) {
            // The PostgreSQL spellings: casts, regex match, JSON containment, path text, key and array overlap.
            out.push(t);
            i += n;
        } else if ch[i..].starts_with(&['<', '=', '>']) {
            // MySQL's NULL-safe equality.
            out.push(T::Cmp("<=>"));
            i += 3;
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

/// The values of `( a , b , c )` or `[ a , b ]` starting at `t[0]`, and how many tokens it took.
fn list_of(t: &[T]) -> Option<(Vec<String>, usize)> {
    let close = match t.first()? {
        T::P('(') => ')',
        T::P('[') => ']',
        _ => return None,
    };
    // Values are single tokens, so the first closing mark ends the list.
    let end = t.iter().position(|x| *x == T::P(close))?;
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
    // PostgreSQL reads uuid, inet and other text-like columns as `col::text` and arrays as `col::text[]`, and MySQL
    // matches case on `CAST(col AS BINARY)`; the column is the same rule. Writing the rule out again (in `parse`) tells
    // whether the cast belonged there.
    let mut cut = Vec::with_capacity(t.len());
    let (mut k, mut bin) = (0, false);
    while k < t.len() {
        if let [cast, T::P('('), id @ T::Id(_), a, b, T::P(')'), ..] = &t[k..] && word(cast, "CAST") && word(a, "AS") && word(b, "BINARY") {
            cut.push(id.clone());
            bin = true;
            k += 6;
            continue;
        }
        cut.push(t[k].clone());
        let text = matches!(t[k], T::Id(_)) && t.get(k + 1) == Some(&T::Cast) && t.get(k + 2).is_some_and(|w| word(w, "TEXT"));
        k += match () {
            _ if text && t.get(k + 3) == Some(&T::P('[')) && t.get(k + 4) == Some(&T::P(']')) => 5,
            _ if text => 3,
            _ => 1,
        };
    }
    let mut r = rule_at(&cut, cols, d)?;
    r.case |= bin;
    // The flag means nothing where it changes no SQL, so it reads back as a new rule would have it.
    if !has_case(r.kind, r.op) {
        r.case = default_case(r.op, d);
    }
    Some(r)
}

fn rule_at(t: &[T], cols: &[(String, Kind)], d: Dialect) -> Option<Rule> {
    let mk = |col: &str, op: Op, values: Vec<String>| Some(Rule { join: false, col: col.into(), kind: kind_of(cols, col)?, op, values, case: default_case(op, d) });
    let cased = |r: Option<Rule>, case: bool| r.map(|r| Rule { case, ..r });
    if d == Dialect::Postgres && let Some(r) = pg_rule_of(t, &mk) {
        return Some(r);
    }
    // Wrapped forms: NOT IN ... OR NULL and this month.
    if t.first() == Some(&T::P('(')) && close_of(t, 0) == Some(t.len() - 1) {
        let inner = &t[1..t.len() - 1];
        if let [T::Id(c), n, i, T::P('('), ..] = inner && word(n, "NOT") && word(i, "IN") {
            let (vals, used) = list_of(&inner[3..])?;
            let rest = &inner[3 + used..];
            if matches!(rest, [o, T::Id(c2), is, nu] if word(o, "OR") && c2 == c && word(is, "IS") && word(nu, "NULL")) {
                return mk(c, Op::NotInOrNull, vals);
            }
        }
        if let [y, T::P('('), T::Id(c), T::P(')'), ..] = inner && word(y, "YEAR") && inner.len() == 23 {
            return mk(c, Op::ThisMonth, vec![]);
        }
        return None;
    }
    // Function forms.
    match t {
        [d, T::P('('), T::Id(c), T::P(')'), T::Cmp("="), cd, T::P('('), T::P(')')] if word(d, "DATE") && word(cd, "CURDATE") => return mk(c, Op::Today, vec![]),
        [d, T::P('('), T::Id(c), T::P(')'), T::Cmp("="), cd, T::P('('), T::P(')'), T::P('-'), iv, T::Num(one), day] if word(d, "DATE") && word(cd, "CURDATE") && word(iv, "INTERVAL") && one == "1" && word(day, "DAY") => return mk(c, Op::Yesterday, vec![]),
        [e, T::P('('), ym, f, T::Id(c), T::P(')'), T::Cmp("="), e2, T::P('('), ym2, f2, cd, T::P('('), T::P(')'), T::P('-'), iv, T::Num(one), mo, T::P(')')]
            if word(e, "EXTRACT") && word(e2, "EXTRACT") && word(ym, "YEAR_MONTH") && word(ym2, "YEAR_MONTH") && word(f, "FROM") && word(f2, "FROM") && word(cd, "CURDATE") && word(iv, "INTERVAL") && one == "1" && word(mo, "MONTH") =>
        {
            return mk(c, Op::LastMonth, vec![]);
        }
        [y, T::P('('), T::Id(c), T::P(','), T::Num(one), T::P(')'), T::Cmp("="), ..] if word(y, "YEARWEEK") && one == "1" => return mk(c, Op::ThisWeek, vec![]),
        [l, T::P('('), T::Id(c), T::P(')'), T::Cmp(o), T::Num(n)] if word(l, "CHAR_LENGTH") || word(l, "LENGTH") => {
            let op = match *o { "=" => Op::LenEq, ">" => Op::LenGt, "<" => Op::LenLt, _ => return None };
            return mk(c, op, vec![n.clone()]);
        }
        [f, T::P('('), T::Str(v), T::P(','), T::Id(c), T::P(')'), T::Cmp(">"), T::Num(z)] if word(f, "FIND_IN_SET") && z == "0" => return mk(c, Op::SetContains, vec![v.clone()]),
        [u, T::P('('), e, T::P('('), T::Id(c), T::P(','), T::Str(p), T::P(')'), T::P(')'), T::Cmp(o @ ("=" | "<>")), T::Str(v)] if word(u, "JSON_UNQUOTE") && word(e, "JSON_EXTRACT") => {
            return mk(c, if *o == "=" { Op::JsonPathEq } else { Op::JsonPathNe }, vec![p.clone(), v.clone()]);
        }
        [e, T::P('('), T::Id(c), T::P(','), T::Str(p), T::P(')'), T::Cmp(o @ (">" | "<")), v] if word(e, "JSON_EXTRACT") => return mk(c, if *o == ">" { Op::JsonPathGt } else { Op::JsonPathLt }, vec![p.clone(), value_of(v)?]),
        [f, T::P('('), T::Id(c), T::P(','), T::Str(v), T::P(')')] if word(f, "JSON_CONTAINS") => return mk(c, Op::JsonContains, vec![v.clone()]),
        [f, T::P('('), T::Id(c), T::P(','), T::Str(one), T::P(','), T::Str(p), T::P(')')] if word(f, "JSON_CONTAINS_PATH") && one == "one" => return mk(c, Op::JsonHasPath, vec![p.clone()]),
        [n, T::P('('), T::Id(c), T::Cmp("<=>"), v, T::P(')')] if word(n, "NOT") => return mk(c, Op::Distinct, vec![value_of(v)?]),
        _ => {}
    }
    let (T::Id(c), rest) = (t.first()?, &t[1..]) else { return None };
    let kind = kind_of(cols, c)?;
    // PostgreSQL's LIKE matches case and ILIKE does not; on MySQL both cases are LIKE, told apart by the cast in `rule_of`.
    let pg = d == Dialect::Postgres;
    let like = |s: &str, neg: bool, case: bool| -> Option<Rule> {
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
        // The keyword is checked too: a LIKE on a uuid's text is the person's own, since the structured form is ILIKE there.
        let probe = Rule { join: false, col: c.clone(), kind, op, values: vec![v.clone()], case };
        let kw = if pg && !case { "ILIKE" } else { "LIKE" };
        if rule_sql(&probe, d, true).ends_with(&format!(" {kw} {}", lit(s, true))) { Some(probe) } else { None }
    };
    match rest {
        [i, nu] if word(i, "IS") && word(nu, "NULL") => mk(c, Op::IsNull, vec![]),
        [i, n, nu] if word(i, "IS") && word(n, "NOT") && word(nu, "NULL") => mk(c, Op::NotNull, vec![]),
        [i, di, f, v] if pg && word(i, "IS") && word(di, "DISTINCT") && word(f, "FROM") => mk(c, Op::Distinct, vec![value_of(v)?]),
        [b, x, a, y] if word(b, "BETWEEN") && word(a, "AND") => mk(c, Op::Between, vec![value_of(x)?, value_of(y)?]),
        [i, ..] if word(i, "IN") => mk(c, Op::In, list_of(&rest[1..])?.0),
        [n, i, ..] if word(n, "NOT") && word(i, "IN") => mk(c, Op::NotIn, list_of(&rest[2..])?.0),
        [l, T::Str(s)] if word(l, "LIKE") => like(s, false, pg).or_else(|| mk(c, Op::Like, vec![s.clone()])),
        [n, l, T::Str(s)] if word(n, "NOT") && word(l, "LIKE") => like(s, true, pg).or_else(|| mk(c, Op::NotLike, vec![s.clone()])),
        [l, T::Str(s)] if pg && word(l, "ILIKE") => like(s, false, false),
        [n, l, T::Str(s)] if pg && word(n, "NOT") && word(l, "ILIKE") => like(s, true, false),
        [r, T::Str(s)] if word(r, "REGEXP") => match s.strip_prefix("(?-i)") {
            Some(p) => cased(mk(c, Op::Regexp, vec![p.into()]), true),
            None => mk(c, Op::Regexp, vec![s.clone()]),
        },
        [n, r, T::Str(s)] if word(n, "NOT") && word(r, "REGEXP") => match s.strip_prefix("(?-i)") {
            Some(p) => cased(mk(c, Op::NotRegexp, vec![p.into()]), true),
            None => mk(c, Op::NotRegexp, vec![s.clone()]),
        },
        [T::Cmp(">="), n, T::P('('), T::P(')'), T::P('-'), iv, T::Num(k), u] if word(n, "NOW") && word(iv, "INTERVAL") => {
            let unit = match u { T::Word(w) if w == "HOUR" => "hours", T::Word(w) if w == "DAY" => "days", T::Word(w) if w == "WEEK" => "weeks", T::Word(w) if w == "MONTH" => "months", _ => return None };
            mk(c, Op::Last, vec![k.clone(), unit.into()])
        }
        [T::Cmp(">"), T::Id(o)] => mk(c, Op::AfterCol, vec![o.clone()]),
        [T::Cmp("="), T::Str(s)] if s.is_empty() && group_of(kind) == Group::Text => mk(c, Op::Empty, vec![]),
        [T::Cmp("<>"), T::Str(s)] if s.is_empty() && group_of(kind) == Group::Text => mk(c, Op::NotEmpty, vec![]),
        [T::Cmp(o @ ("=" | "<>" | ">" | "<" | ">=" | "<=")), v] => {
            let op = match *o { "=" => Op::Eq, "<>" => Op::Ne, ">" => Op::Gt, "<" => Op::Lt, ">=" => Op::Ge, _ => Op::Le };
            // PostgreSQL's = matches case; on MySQL a bare = goes by the collation (see `default_case`).
            cased(mk(c, op, vec![value_of(v)?]), pg)
        }
        _ => None,
    }
}

/// The forms `pg_rule_sql` writes, read back. `t` has its `::text` casts after a column already cut.
fn pg_rule_of(t: &[T], mk: &impl Fn(&str, Op, Vec<String>) -> Option<Rule>) -> Option<Rule> {
    let jsonb = |a: &T, b: &T| *a == T::Cast && word(b, "JSONB");
    let cased = |r: Option<Rule>, case: bool| r.map(|r| Rule { case, ..r });
    let path_first = |t: &[T]| -> Option<(String, String)> {
        match t {
            [f, T::P('('), T::Id(c), x, y, T::P(','), T::Str(p), T::P(')'), T::Cmp("#>>"), T::Str(all)] if word(f, "JSONB_PATH_QUERY_FIRST") && jsonb(x, y) && all == "{}" => Some((c.clone(), p.clone())),
            _ => None,
        }
    };
    let len_op = |o: &str| match o { "=" => Some(Op::LenEq), ">" => Some(Op::LenGt), "<" => Some(Op::LenLt), _ => None };
    let month = |g: &str, g2: &str| g == "month" && g2 == "month";
    if let [rest @ .., T::Cmp(o @ ("=" | "<>")), T::Str(v)] = t && let Some((c, p)) = path_first(rest) {
        return mk(&c, if *o == "=" { Op::JsonPathEq } else { Op::JsonPathNe }, vec![p, v.clone()]);
    }
    if let [T::P('('), inner @ .., T::P(')'), T::Cast, nu, T::Cmp(o @ (">" | "<")), v] = t && word(nu, "NUMERIC") && let Some((c, p)) = path_first(inner) {
        return mk(&c, if *o == ">" { Op::JsonPathGt } else { Op::JsonPathLt }, vec![p, value_of(v)?]);
    }
    match t {
        [T::Id(c), T::Cast, dt, T::Cmp("="), cd] if word(dt, "DATE") && word(cd, "CURRENT_DATE") => mk(c, Op::Today, vec![]),
        [T::Id(c), T::Cast, dt, T::Cmp("="), cd, T::P('-'), T::Num(one)] if word(dt, "DATE") && word(cd, "CURRENT_DATE") && one == "1" => mk(c, Op::Yesterday, vec![]),
        [T::Id(c), T::Cmp(o @ ("~" | "~*")), T::Str(v)] => cased(mk(c, Op::Regexp, vec![v.clone()]), *o == "~"),
        [T::Id(c), T::Cmp(o @ ("!~" | "!~*")), T::Str(v)] => cased(mk(c, Op::NotRegexp, vec![v.clone()]), *o == "!~"),
        [lo, T::P('('), T::Id(c), T::P(')'), T::Cmp(o @ ("=" | "<>")), lo2, T::P('('), T::Str(v), T::P(')')] if word(lo, "LOWER") && word(lo2, "LOWER") => cased(mk(c, if *o == "=" { Op::Eq } else { Op::Ne }, vec![v.clone()]), false),
        [T::Id(c), T::Cmp(">="), n, T::P('('), T::P(')'), T::P('-'), iv, T::Str(s)] if word(n, "NOW") && word(iv, "INTERVAL") => {
            let (k, u) = s.split_once(' ')?;
            let unit = match u { "HOUR" => "hours", "DAY" => "days", "WEEK" => "weeks", "MONTH" => "months", _ => return None };
            mk(c, Op::Last, vec![k.into(), unit.into()])
        }
        [f, T::P('('), T::Str(g), T::P(','), T::Id(c), T::P(')'), T::Cmp("="), f2, T::P('('), T::Str(g2), T::P(','), cd, T::P(')')] if word(f, "DATE_TRUNC") && word(f2, "DATE_TRUNC") && g == g2 && word(cd, "CURRENT_DATE") => match g.as_str() {
            "week" => mk(c, Op::ThisWeek, vec![]),
            "month" => mk(c, Op::ThisMonth, vec![]),
            _ => None,
        },
        [f, T::P('('), T::Str(g), T::P(','), T::Id(c), T::P(')'), T::Cmp("="), f2, T::P('('), T::Str(g2), T::P(','), cd, T::P(')'), T::P('-'), iv, T::Str(one)]
            if word(f, "DATE_TRUNC") && word(f2, "DATE_TRUNC") && month(g, g2) && word(cd, "CURRENT_DATE") && word(iv, "INTERVAL") && one == "1 month" =>
        {
            mk(c, Op::LastMonth, vec![])
        }
        [T::Str(v), T::Cmp("="), a, T::P('('), f, T::P('('), T::Id(c), T::P(','), T::Str(sep), T::P(')'), T::P(')')] if word(a, "ANY") && word(f, "STRING_TO_ARRAY") && sep == "," => mk(c, Op::SetContains, vec![v.clone()]),
        [T::Id(c), x, y, T::Cmp("@>"), T::Str(v), x2, y2] if jsonb(x, y) && jsonb(x2, y2) => mk(c, Op::JsonContains, vec![v.clone()]),
        [T::Id(c), x, y, T::Cmp("?"), T::Str(k)] if jsonb(x, y) => mk(c, Op::JsonHasKey, vec![k.clone()]),
        [f, T::P('('), T::Id(c), x, y, T::P(','), T::Str(p), T::P(')')] if word(f, "JSONB_PATH_EXISTS") && jsonb(x, y) => mk(c, Op::JsonHasPath, vec![p.clone()]),
        [T::Id(c), T::Cmp(o @ ("=" | "<>")), T::Str(v), x, y] if jsonb(x, y) => mk(c, if *o == "=" { Op::Eq } else { Op::Ne }, vec![v.clone()]),
        [T::Id(c), T::Cmp(o @ ("@>" | "&&")), a, rest @ ..] if word(a, "ARRAY") => {
            let (items, used) = list_of(rest)?;
            (used == rest.len()).then_some(())?;
            mk(c, if *o == "@>" { Op::ArrayHasAll } else { Op::ArrayHasAny }, items)
        }
        [f, T::P('('), T::Id(c), T::P(')'), T::Cmp(o), T::Num(n)] if word(f, "CARDINALITY") => {
            let op = match len_op(o)? { Op::LenEq => Op::ArrayLenEq, Op::LenGt => Op::ArrayLenGt, _ => Op::ArrayLenLt };
            mk(c, op, vec![n.clone()])
        }
        [f, T::P('('), T::Id(c), T::P(')'), T::Cmp(o), T::Num(n)] if word(f, "OCTET_LENGTH") => mk(c, len_op(o)?, vec![n.clone()]),
        _ => None,
    }
}

/// Reads text back into rules when it is an AND or OR list of what `to_sql` writes, with groups in parentheses one level
/// deep. Returns the rules and whether they are joined with OR. Anything else (subqueries, mixed AND/OR, other
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
    let mut rules = Vec::new();
    for p in &parts {
        // A rule may be wrapped in parentheses itself (NOT IN ... OR NULL), so that is tried before a group.
        if let Some(r) = rule_of(p, cols, d) {
            rules.push(r);
            continue;
        }
        if parts.len() < 2 || p.first() != Some(&T::P('(')) || close_of(p, 0) != Some(p.len() - 1) {
            return None;
        }
        let (inner, inner_any) = split(&p[1..p.len() - 1])?;
        if inner.len() < 2 || inner_any == any {
            return None;
        }
        for (k, q) in inner.iter().enumerate() {
            rules.push(Rule { join: k > 0, ..rule_of(q, cols, d)? });
        }
    }
    let any = any && rules.len() > 1;
    if !rules.iter().all(|r| ops_for(r.kind, d).contains(&r.op) && r.complete()) {
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

    #[test]
    fn filter_help_shows_each_dialects_own_operators() {
        let pg = filter_help(Dialect::Postgres);
        let my = filter_help(Dialect::MySql);
        assert!(pg.contains("\"name\" ILIKE '%paid%'") && pg.contains("\"name\"::text ~* '^[0-9]+$'") && pg.contains("\"name\" LIKE '%paid%'"), "{pg}");
        assert!(my.contains("`name` LIKE '%paid%'") && my.contains("`name` REGEXP '^[0-9]+$'") && my.contains("CAST(`name` AS BINARY) LIKE") && !my.contains("ILIKE"), "{my}");
    }

    fn cols() -> Vec<(String, Kind)> {
        [("id", Kind::Int), ("total", Kind::Decimal), ("name", Kind::Text), ("created_at", Kind::DateTime), ("status", Kind::Enum), ("tags", Kind::Set), ("data", Kind::Json), ("blob", Kind::Binary), ("seen_at", Kind::DateTime)]
            .iter()
            .map(|(n, k)| (n.to_string(), *k))
            .collect()
    }

    /// A rule as a new one of this dialect would have it.
    fn rule(col: &str, kind: Kind, op: Op, values: &[&str], d: Dialect) -> Rule {
        Rule { join: false, col: col.into(), kind, op, values: values.iter().map(|v| v.to_string()).collect(), case: default_case(op, d) }
    }

    fn vals(op: Op) -> Vec<String> {
        use Op::*;
        match op {
            Between => vec!["a".into(), "b".into()],
            In | NotIn | NotInOrNull | ArrayHasAll | ArrayHasAny => vec!["x".into(), "it's".into(), "back\\slash".into()],
            Last => vec!["3".into(), "weeks".into()],
            JsonPathEq | JsonPathNe | JsonPathGt | JsonPathLt => vec!["$.a.b".into(), "v".into()],
            AfterCol => vec!["seen_at".into()],
            LenEq | LenGt | LenLt | ArrayLenEq | ArrayLenGt | ArrayLenLt => vec!["5".into()],
            Contains | NotContains | StartsWith | NotStartsWith | EndsWith | NotEndsWith => vec!["50%_off\\".into()],
            SetContains => vec!["a".into()],
            _ => default_values(op).into_iter().map(|_| "ab'c".to_string()).collect(),
        }
    }

    /// The values `vals` gives, fitted to the column: numbers in number columns, hex in binary ones.
    fn fitted(op: Op, kind: Kind) -> Vec<String> {
        use Op::*;
        let g = group_of(kind);
        match op {
            Eq | Ne | Gt | Lt | Ge | Le | Distinct if g == Group::Number => vec!["50.5".into()],
            Eq | Ne if g == Group::Other => vec!["0x5A3F9C".into()],
            Between if g == Group::Number => vec!["50".into(), "200".into()],
            In | NotInOrNull if g == Group::Number => vec!["1".into(), "2".into()],
            JsonPathGt | JsonPathLt => vec!["$.n".into(), "5".into()],
            _ => vals(op),
        }
    }

    /// Every rule the bar writes on MySQL and MariaDB reads back into the same rule, with case matched or not.
    #[test]
    fn every_op_round_trips() {
        for d in [Dialect::MySql, Dialect::MariaDb] {
            for (col, kind) in cols() {
                for &op in ops_for(kind, d) {
                    if matches!(op, Op::AfterCol) && col != "created_at" {
                        continue;
                    }
                    for case in [false, true] {
                        let r = Rule { case: if has_case(kind, op) { case } else { default_case(op, d) }, ..rule(&col, kind, op, &[], d) };
                        let r = Rule { values: fitted(op, kind), ..r };
                        assert!(r.complete(), "{op:?}");
                        let sql = to_sql(std::slice::from_ref(&r), false, d, true);
                        let (back, any) = parse(&sql, &cols(), d, true).unwrap_or_else(|| panic!("{d:?}: no parse for {op:?} on {col}: {sql}"));
                        assert!(!any);
                        assert_eq!(back, vec![r], "{d:?}: {sql}");
                    }
                }
            }
        }
    }

    /// Every rule the bar writes on PostgreSQL reads back into the same rule, so the bar stays in rules mode.
    #[test]
    fn every_op_round_trips_on_postgres() {
        let d = Dialect::Postgres;
        let mut cols = cols();
        cols.push(("u".into(), Kind::Uuid));
        cols.push(("arr".into(), Kind::Array));
        for (col, kind) in cols.clone() {
            for &op in ops_for(kind, d) {
                // A bytea value is written as E'\\x..', and text with a backslash stays SQL on a server without backslash escapes.
                if kind == Kind::Binary && matches!(op, Op::Eq | Op::Ne) {
                    continue;
                }
                if matches!(op, Op::AfterCol) && col != "created_at" {
                    continue;
                }
                let mut v: Vec<String> = fitted(op, kind).into_iter().map(|x| x.replace('\\', "")).collect();
                // LIKE escapes % and _ with a backslash, which keeps such text in SQL mode on PostgreSQL (see `parse`).
                if matches!(op, Op::Contains | Op::NotContains | Op::StartsWith | Op::NotStartsWith | Op::EndsWith | Op::NotEndsWith) {
                    v = vec!["ab'c".into()];
                }
                if kind == Kind::Json && matches!(op, Op::Eq | Op::Ne) {
                    v = vec!["{\"a\": 1}".into()];
                }
                for case in [false, true] {
                    let r = Rule { case: if has_case(kind, op) { case } else { default_case(op, d) }, values: v.clone(), ..rule(&col, kind, op, &[], d) };
                    assert!(r.complete(), "{op:?}");
                    let sql = to_sql(std::slice::from_ref(&r), false, d, false);
                    let (back, _) = parse(&sql, &cols, d, false).unwrap_or_else(|| panic!("no parse for {op:?} on {col}: {sql}"));
                    assert_eq!(back, vec![r], "{sql}");
                }
            }
        }
    }

    #[test]
    fn case_and_regex_spellings_per_engine() {
        let sql = |op: Op, v: &str, case: bool, d: Dialect| rule_sql(&Rule { case, ..rule("name", Kind::Text, op, &[v], d) }, d, true);
        let pg = Dialect::Postgres;
        assert_eq!(sql(Op::Eq, "Ann", false, pg), "lower(\"name\") = lower('Ann')");
        assert_eq!(sql(Op::Ne, "Ann", true, pg), "\"name\" <> 'Ann'");
        assert_eq!(sql(Op::Contains, "ann", false, pg), "\"name\" ILIKE '%ann%'");
        assert_eq!(sql(Op::NotStartsWith, "ann", true, pg), "\"name\" NOT LIKE 'ann%'");
        assert_eq!(sql(Op::Regexp, "^a", false, pg), "\"name\"::text ~* '^a'");
        assert_eq!(sql(Op::NotRegexp, "^a", true, pg), "\"name\"::text !~ '^a'");
        for d in [Dialect::MySql, Dialect::MariaDb] {
            assert_eq!(sql(Op::Eq, "Ann", false, d), "`name` = 'Ann'");
            assert_eq!(sql(Op::Eq, "Ann", true, d), "CAST(`name` AS BINARY) = 'Ann'");
            assert_eq!(sql(Op::EndsWith, "nn", true, d), "CAST(`name` AS BINARY) LIKE '%nn'");
            assert_eq!(sql(Op::Regexp, "^a", true, d), "`name` REGEXP '(?-i)^a'");
            assert_eq!(sql(Op::NotRegexp, "^a", false, d), "`name` NOT REGEXP '^a'");
        }
        // Other columns have no flag: a uuid's text is searched without regard to case as before.
        let u = Rule { case: true, ..rule("u", Kind::Uuid, Op::Contains, &["ab"], pg) };
        assert_eq!(rule_sql(&u, pg, false), "\"u\"::text ILIKE '%ab%'");
        // New operators.
        assert_eq!(rule_sql(&rule("n", Kind::Int, Op::Distinct, &["5"], pg), pg, false), "\"n\" IS DISTINCT FROM 5");
        assert_eq!(rule_sql(&rule("n", Kind::Int, Op::Distinct, &["5"], Dialect::MySql), Dialect::MySql, true), "NOT (`n` <=> 5)");
        assert_eq!(rule_sql(&rule("a", Kind::Array, Op::ArrayHasAll, &["x", "y"], pg), pg, false), "\"a\"::text[] @> ARRAY['x', 'y']");
        assert_eq!(rule_sql(&rule("a", Kind::Array, Op::ArrayLenGt, &["2"], pg), pg, false), "cardinality(\"a\") > 2");
        assert_eq!(rule_sql(&rule("j", Kind::Json, Op::JsonHasKey, &["k"], pg), pg, false), "\"j\"::jsonb ? 'k'");
        assert_eq!(rule_sql(&rule("j", Kind::Json, Op::JsonPathGt, &["$.n", "5"], pg), pg, false), "(jsonb_path_query_first(\"j\"::jsonb, '$.n') #>> '{}')::numeric > 5");
        assert!(!ops_for(Kind::Json, Dialect::MySql).contains(&Op::JsonHasKey) && ops_for(Kind::Json, pg).contains(&Op::JsonHasKey));
        assert_eq!(op_sql_hint(Op::Contains, Kind::Text, pg, false), "x ILIKE '%…%'");
    }

    /// Text the bar wrote before operators had a case flag reads back into rules and writes the same text again.
    #[test]
    fn old_spellings_read_back_unchanged() {
        let my = [
            "`name` = 'ann'",
            "`name` LIKE '%ann%'",
            "`name` NOT LIKE 'ann%'",
            "`name` REGEXP '^a'",
            "`total` > 50 AND `name` <> 'x'",
            "(`name` NOT IN ('a') OR `name` IS NULL)",
            "(YEAR(`created_at`) = YEAR(CURDATE()) AND MONTH(`created_at`) = MONTH(CURDATE()))",
            "JSON_UNQUOTE(JSON_EXTRACT(`data`, '$.a')) = 'v'",
        ];
        for text in my {
            let (rules, any) = parse(text, &cols(), Dialect::MySql, true).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(to_sql(&rules, any, Dialect::MySql, true), text);
            assert!(rules.iter().all(|r| !r.case), "{text}");
        }
        let pg = [
            "\"name\" = 'ann'",
            "\"name\" ILIKE '%ann%'",
            "\"name\"::text ~ '^a'",
            "\"name\"::text !~ '^a'",
            "\"name\" LIKE '%ann%'",
            "\"name\" LIKE 'a%b'",
            "\"created_at\"::date = CURRENT_DATE",
            "date_trunc('month', \"created_at\") = date_trunc('month', CURRENT_DATE)",
            "jsonb_path_query_first(\"data\"::jsonb, '$.a') #>> '{}' = 'v'",
        ];
        for text in pg {
            let (rules, any) = parse(text, &cols(), Dialect::Postgres, false).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(to_sql(&rules, any, Dialect::Postgres, false), text);
        }
        // What they read as: = and regex as typed match case, ILIKE does not, a structured LIKE matches case and any other stays LIKE.
        let one = |t: &str| parse(t, &cols(), Dialect::Postgres, false).unwrap().0.remove(0);
        assert!(one("\"name\" = 'ann'").case && one("\"name\"::text ~ '^a'").case && !one("\"name\" ILIKE '%ann%'").case);
        assert_eq!((one("\"name\" LIKE '%ann%'").op, one("\"name\" LIKE '%ann%'").case), (Op::Contains, true));
        assert_eq!(one("\"name\" LIKE 'a%b'").op, Op::Like);
    }

    #[test]
    fn lexer_reads_the_new_operators() {
        let c = [("a".to_string(), Kind::Array), ("j".to_string(), Kind::Json), ("n".to_string(), Kind::Int)];
        let toks = lex("a::text[] && ARRAY['x'] j ? 'k' n <=> 1 n ~* 'p' n !~* 'p' n <= 2", &c, Dialect::Postgres).unwrap();
        for want in [T::Cmp("&&"), T::P('['), T::P(']'), T::Cmp("?"), T::Cmp("<=>"), T::Cmp("~*"), T::Cmp("!~*"), T::Cmp("<=")] {
            assert!(toks.contains(&want), "{want:?} in {toks:?}");
        }
        assert!(!toks.contains(&T::Cmp("~")) && !toks.contains(&T::Cmp("!~")));
    }

    #[test]
    fn rules_save_as_json_without_their_kind() {
        let r = Rule { case: true, ..rule("name", Kind::Text, Op::Contains, &["a"], Dialect::MySql) };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("kind") && !json.contains("join"), "{json}");
        let back: Rule = serde_json::from_str(&json).unwrap();
        assert_eq!((back.op, back.case, back.values), (Op::Contains, true, vec!["a".to_string()]));
    }

    #[test]
    fn like_names_its_escape_without_backslash_escapes() {
        let r = rule("name", Kind::Text, Op::Contains, &["50%"], Dialect::MySql);
        assert_eq!(rule_sql(&r, Dialect::MySql, false), "`name` LIKE '%50\\%%' ESCAPE '\\'");
        assert_eq!(rule_sql(&r, Dialect::MySql, true), "`name` LIKE '%50\\\\%%'");
        let plain = Rule { values: vec!["ann".into()], ..r };
        assert_eq!(rule_sql(&plain, Dialect::MySql, false), "`name` LIKE '%ann%'");
    }

    #[test]
    fn postgres_rules_use_postgres_operators() {
        let sql = |col: &str, kind: Kind, op: Op, values: &[&str]| rule_sql(&Rule { case: true, ..rule(col, kind, op, values, Dialect::Postgres) }, Dialect::Postgres, false);
        assert_eq!(sql("name", Kind::Text, Op::Regexp, &["^a"]), "\"name\"::text ~ '^a'");
        assert_eq!(sql("created_at", Kind::DateTime, Op::Today, &[]), "\"created_at\"::date = CURRENT_DATE");
        assert_eq!(sql("created_at", Kind::DateTime, Op::Yesterday, &[]), "\"created_at\"::date = CURRENT_DATE - 1");
        assert_eq!(sql("created_at", Kind::DateTime, Op::Last, &["3", "weeks"]), "\"created_at\" >= NOW() - INTERVAL '3 WEEK'");
        assert_eq!(sql("data", Kind::Json, Op::JsonHasPath, &["$.a.b"]), "jsonb_path_exists(\"data\"::jsonb, '$.a.b')");
        for op in [Op::Regexp, Op::Today, Op::Yesterday, Op::ThisWeek, Op::ThisMonth, Op::LastMonth, Op::JsonContains, Op::Distinct] {
            let s = sql("c", Kind::Text, op, &["x"]);
            assert!(!s.contains("REGEXP") && !s.contains("CURDATE") && !s.contains("YEARWEEK") && !s.contains("JSON_") && !s.contains("<=>"), "{s}");
        }
    }

    #[test]
    fn postgres_rules_round_trip_through_double_quoted_names() {
        let d = Dialect::Postgres;
        let a = rule("status", Kind::Enum, Op::In, &["paid", "shipped"], d);
        let b = rule("to\"tal", Kind::Decimal, Op::Gt, &["50"], d);
        let sql = to_sql(&[a.clone(), b.clone()], false, d, false);
        assert_eq!(sql, "\"status\" IN ('paid', 'shipped') AND \"to\"\"tal\" > 50");
        assert_eq!(parse(&sql, &[("status".into(), Kind::Enum), ("to\"tal".into(), Kind::Decimal)], d, false), Some((vec![a, b], false)));
        // A backtick name is not a name on PostgreSQL, and a double quoted one is not on MySQL.
        assert!(parse("`status` = 'paid'", &cols(), d, false).is_none());
        assert!(parse("\"status\" = 'paid'", &cols(), Dialect::MySql, true).is_none());
    }

    #[test]
    fn groups_join_by_the_other_word_in_parentheses_on_every_engine() {
        for d in [Dialect::MySql, Dialect::MariaDb, Dialect::Postgres] {
            let r = |col: &str, kind: Kind, op: Op, v: &str, join: bool| Rule { join, ..rule(col, kind, op, &[v], d) };
            let a = r("total", Kind::Decimal, Op::Gt, "50", false);
            let b = r("status", Kind::Enum, Op::Eq, "paid", false);
            let c = r("name", Kind::Text, Op::Eq, "ann", true);
            let list = vec![a.clone(), b.clone(), c.clone()];
            for any in [false, true] {
                let sql = to_sql(&list, any, d, true);
                let (outer, inner) = if any { ("OR", "AND") } else { ("AND", "OR") };
                assert!(sql.contains(&format!(" {outer} (")) && sql.contains(&format!(" {inner} ")) && sql.ends_with(')'), "{d:?}: {sql}");
                assert_eq!(parse(&sql, &cols(), d, true), Some((list.clone(), any)), "{d:?}: {sql}");
            }
            // A group first reads back too.
            let first = vec![b.clone(), c.clone(), Rule { join: false, ..a.clone() }];
            let sql = to_sql(&first, false, d, true);
            assert_eq!(parse(&sql, &cols(), d, true), Some((first, false)), "{d:?}: {sql}");
            if d == Dialect::MySql {
                // One group alone is a plain list in its own word.
                assert_eq!(to_sql(&[b.clone(), c.clone()], false, d, true), "`status` = 'paid' OR `name` = 'ann'");
                // A rule wrapped in parentheses of its own is still one rule, not a group.
                let nin = rule("name", Kind::Text, Op::NotInOrNull, &["ann"], d);
                let sql = to_sql(&[a.clone(), nin.clone()], false, d, true);
                assert_eq!(parse(&sql, &cols(), d, true), Some((vec![a.clone(), nin], false)), "{sql}");
            }
        }
        // The same word inside and out, a group alone in parentheses, or two levels stay SQL.
        for text in ["total > 50 AND (status = 'paid' AND name = 'ann')", "(status = 'paid' OR name = 'ann')", "total > 50 AND (status = 'paid' OR (name = 'ann' AND id = 1))"] {
            assert!(parse(text, &cols(), Dialect::MySql, true).is_none(), "{text}");
        }
    }

    #[test]
    fn switching_and_removing_keeps_groups_whole() {
        let r = |col: &str, join: bool| Rule { join, ..rule(col, Kind::Int, Op::Eq, &["1"], Dialect::MySql) };
        // Every rule joined by the other word is a plain list in it.
        let (mut list, mut any) = (vec![r("a", false), r("b", true), r("c", true)], false);
        normalize(&mut list, &mut any);
        assert!(any && list.iter().all(|x| !x.join));
        // a AND (b OR c): removing b leaves c to start its own group, not to join a.
        let (mut list, mut any) = (vec![r("a", false), r("b", false), r("c", true)], false);
        remove(&mut list, &mut any, 1);
        assert_eq!((list.iter().map(|x| (x.col.as_str(), x.join)).collect::<Vec<_>>(), any), (vec![("a", false), ("c", false)], false));
        // Removing a leaves (b OR c) alone, which is the list b OR c.
        let (mut list, mut any) = (vec![r("a", false), r("b", false), r("c", true)], false);
        remove(&mut list, &mut any, 0);
        assert_eq!((list.iter().map(|x| (x.col.as_str(), x.join)).collect::<Vec<_>>(), any), (vec![("b", false), ("c", false)], true));
    }

    #[test]
    fn lists_join_with_and_or_or_and_reject_mixes() {
        let a = rule("status", Kind::Enum, Op::In, &["paid", "shipped"], Dialect::MySql);
        let b = rule("total", Kind::Decimal, Op::Gt, &["50"], Dialect::MySql);
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
        let d = Dialect::MySql;
        assert_eq!(rule_sql(&rule("name", Kind::Text, Op::Eq, &["5"], d), d, true), "`name` = '5'");
        assert_eq!(rule_sql(&rule("total", Kind::Decimal, Op::Eq, &["5 OR 1=1"], d), d, true), "`total` = '5 OR 1=1'");
        let q = rule("na`me", Kind::Text, Op::Eq, &["a'\\"], d);
        assert_eq!(rule_sql(&q, d, true), "`na``me` = 'a''\\\\'");
        // Without backslash escapes the backslash is a plain character, and text with one stays SQL.
        assert_eq!(rule_sql(&q, d, false), "`na``me` = 'a''\\'");
        assert!(parse("name = 'a\\b'", &cols(), d, false).is_none());
    }

    #[test]
    fn incomplete_rules_are_left_out() {
        let r = Rule::new("total", Kind::Decimal, Dialect::MySql);
        assert!(!r.complete());
        assert_eq!(to_sql(&[r], false, Dialect::MySql, true), "");
    }

    #[test]
    fn grains() {
        assert_eq!(Grain::Day.expr("created_at", Dialect::MySql), "DATE(`created_at`)");
        assert_eq!(Grain::Week.expr("c", Dialect::MySql), "YEARWEEK(`c`, 3)");
    }
}
