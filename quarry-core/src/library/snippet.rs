//! Snippets: SQL scripts with values to fill in, written `{{name}}`, `{{name:type}}` or `{{name:type=default}}` in the
//! statement itself, so nothing about them is stored apart from the script. In code a value goes in as a literal of its
//! type; inside a string, as in `LIKE '%{{name}}%'`, it joins that string with its quotes escaped. Never as SQL text. In
//! a comment, a quoted name or a dollar-quoted body a placeholder is plain text.
//!
//! Mongo scripts take the same placeholders: Shell JavaScript and the Documents bar's query text, where a value goes in
//! as a JavaScript literal (a JSON string, a number, an array, an ISODate).
use crate::db::sql::lex::{tokens, Tok};
use crate::db::sql::value::{lit, mysql_escapes, pg_lit};
use crate::db::sql::Dialect;
use std::ops::Range;

/// The language a script is written in, which decides where placeholders can sit and how values are written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lang {
    Sql(Dialect),
    /// Shell JavaScript. Values go in code or `${ }` only: a JS string there may be SQL text for an attached connection,
    /// which JavaScript escaping can't keep a value inside.
    Shell,
    /// The Documents bar: JSON5 with shell helpers, as `db::parse_value` reads it. Values go in code and in strings.
    Query,
}

impl From<Dialect> for Lang {
    fn from(d: Dialect) -> Self {
        Lang::Sql(d)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Text,
    Int,
    Number,
    Date,
    DateTime,
    IntList,
    Ident,
    Choice(Vec<String>),
}

/// The types offered by completion, with what each one sends.
pub const TYPES: [(&str, &str); 8] = [
    ("date", "a day, sent as a quoted date"),
    ("datetime", "a moment, with a time"),
    ("text", "any text, quoted"),
    ("int", "a whole number"),
    ("number", "a decimal number"),
    ("int list", "1, 2, 3 for IN ( )"),
    ("a|b|c", "one of the choices you list"),
    ("identifier", "a table or column name, quoted as one"),
];

impl Ty {
    pub fn read(s: &str) -> Option<Ty> {
        Some(match s {
            "text" => Ty::Text,
            "int" => Ty::Int,
            "number" => Ty::Number,
            "date" => Ty::Date,
            "datetime" => Ty::DateTime,
            "int list" => Ty::IntList,
            "identifier" => Ty::Ident,
            _ if s.contains('|') => {
                let c: Vec<String> = s.split('|').map(|c| c.trim().to_string()).collect();
                if c.iter().any(|c| c.is_empty()) {
                    return None;
                }
                Ty::Choice(c)
            }
            _ => return None,
        })
    }

    /// How the type is written after the colon.
    pub fn spec(&self) -> String {
        match self {
            Ty::Text => "text".into(),
            Ty::Int => "int".into(),
            Ty::Number => "number".into(),
            Ty::Date => "date".into(),
            Ty::DateTime => "datetime".into(),
            Ty::IntList => "int list".into(),
            Ty::Ident => "identifier".into(),
            Ty::Choice(c) => c.join("|"),
        }
    }

    /// What an empty field shows for this type.
    pub fn hint(&self) -> &'static str {
        match self {
            Ty::Date => "2026-11-03",
            Ty::DateTime => "2026-11-03 14:30",
            Ty::IntList => "4405, 4407",
            _ => "",
        }
    }

    /// A short word for the form, under the value's name.
    pub fn label(&self) -> String {
        match self {
            Ty::Choice(c) => format!("one of {}", c.len()),
            t => t.spec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub name: String,
    pub ty: Ty,
    pub default: Option<String>,
}

/// Where a placeholder sits: in code, or inside a string closed by that quote, or inside a PostgreSQL `E'...'` string.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum At {
    Code,
    Str(char),
    EStr,
}

/// One placeholder in the text: which value it takes and the bytes it covers, braces included.
#[derive(Clone, Debug, PartialEq)]
pub struct Hole {
    pub var: usize,
    pub span: Range<usize>,
    pub at: At,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VarErr {
    pub span: Range<usize>,
    pub msg: String,
}

/// The values of a text, in the order they first appear. A placeholder that does not read stops the parse at its
/// error, so the holes before it are still there for highlighting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub vars: Vec<Var>,
    pub holes: Vec<Hole>,
    pub err: Option<VarErr>,
}

impl Parsed {
    /// A snippet has values, or at least a placeholder that does not read yet.
    pub fn is_snippet(&self) -> bool {
        !self.holes.is_empty() || self.err.is_some()
    }
}

/// Whether a script's SQL has `{{...}}` values. Plain SQL costs one scan for `{{`.
pub fn is_snippet(body: &str, l: impl Into<Lang>) -> bool {
    body.contains("{{") && parse(body, l).is_snippet()
}

/// Whether byte `i` is code, outside strings, quoted names and comments, and the strings a placeholder can sit in: their
/// insides, between the quotes, and how they quote.
fn code_mask(text: &str, l: Lang) -> (Vec<bool>, Vec<(Range<usize>, At)>) {
    let Lang::Sql(d) = l else { return js_mask(text, l == Lang::Shell) };
    let mut m = vec![false; text.len()];
    let mut strs = Vec::new();
    for (t, r) in tokens(text, d) {
        if matches!(t, Tok::Word | Tok::Punct | Tok::Space | Tok::ExecOpen | Tok::ExecClose) {
            m[r].fill(true);
        } else if t == Tok::Str
            && let Some((open, at)) = string_at(&text[r.clone()], d)
        {
            strs.push((r.start + open..r.end - 1, at));
        }
    }
    (m, strs)
}

/// `code_mask` for JavaScript, or for query text when `shell` is false, which has no templates. A '...' or "..." string
/// ends at its line's end as in JavaScript, so a regex this does not read can only throw off the line it is on.
/// Templates are text with their `${ }` read as code.
fn js_mask(text: &str, shell: bool) -> (Vec<bool>, Vec<(Range<usize>, At)>) {
    let b = text.as_bytes();
    let mut m = vec![false; b.len()];
    let mut strs = Vec::new();
    // The brace depth of each open `${`, innermost last, and the depth now.
    let (mut tpl, mut depth) = (Vec::<usize>::new(), 0usize);
    // A template's text from `from`, up to its closing backtick or next `${`: where code starts again after it.
    let run = |from: usize, strs: &mut Vec<(Range<usize>, At)>, tpl: &mut Vec<usize>, depth: &mut usize| {
        let mut j = from;
        while j < b.len() && b[j] != b'`' && !(b[j] == b'$' && b.get(j + 1) == Some(&b'{')) {
            j += if b[j] == b'\\' { 2 } else { 1 };
        }
        let j = j.min(b.len());
        strs.push((from..j, At::Str('`')));
        if b.get(j) == Some(&b'$') {
            *depth += 1;
            tpl.push(*depth);
            j + 2
        } else {
            j + 1
        }
    };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => i = text[i..].find('\n').map_or(b.len(), |e| i + e),
            b'/' if b.get(i + 1) == Some(&b'*') => i = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2),
            q @ (b'\'' | b'"') => {
                let mut j = i + 1;
                while j < b.len() && b[j] != q && b[j] != b'\n' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let j = j.min(b.len());
                strs.push((i + 1..j, At::Str(q as char)));
                i = j + 1;
            }
            b'`' if shell => i = run(i + 1, &mut strs, &mut tpl, &mut depth),
            b'}' if tpl.last() == Some(&depth) => {
                tpl.pop();
                depth -= 1;
                i = run(i + 1, &mut strs, &mut tpl, &mut depth);
            }
            c => {
                match c {
                    b'{' => depth += 1,
                    b'}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                m[i] = true;
                i += 1;
            }
        }
    }
    (m, strs)
}

/// Whether a string token can hold placeholders: how long its opening is and how it quotes. Prefixed strings other than
/// E'..', and dollar quotes, have no escape a value could rely on, so their braces stay text. So does a plain string with
/// a backslash, which ends elsewhere when the server reads backslashes the other way, and would put the value in code.
pub fn string_at(s: &str, d: Dialect) -> Option<(usize, At)> {
    let plain = !s.contains('\\');
    match s.as_bytes() {
        [b'\'', _, ..] if plain => Some((1, At::Str('\''))),
        [b'"', _, ..] if plain && d != Dialect::Postgres => Some((1, At::Str('"'))),
        [b'E' | b'e', b'\'', ..] if d == Dialect::Postgres => Some((2, At::EStr)),
        _ => None,
    }
}

/// A value name: letters, digits and _, not starting with a digit.
pub fn valid_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn parse(text: &str, l: impl Into<Lang>) -> Parsed {
    let l = l.into();
    let mut out = Parsed::default();
    if !text.contains("{{") {
        return out;
    }
    let (code, strs) = code_mask(text, l);
    let mut typed = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find("{{") {
        let s = i + off;
        let (at, end) = if code[s] && code[s + 1] {
            (At::Code, text.len())
        } else if let Some((r, at)) = strs.iter().find(|(r, _)| r.contains(&s)) {
            // Inside a string the placeholder must close before the string does, or it is just braces in the text.
            if !text[s + 2..r.end].contains("}}") {
                i = s + 2;
                continue;
            }
            (*at, r.end)
        } else {
            i = s + 2;
            continue;
        };
        let Some(len) = text[s + 2..end].find("}}") else {
            out.err = Some(VarErr { span: s..text.len(), msg: "This {{ has no closing }}.".into() });
            return out;
        };
        let e = s + 2 + len + 2;
        let clean = match at {
            At::Code => code[s..e].iter().all(|c| *c),
            _ => !text[s..e].contains(['\'', '"', '\\', '`']),
        };
        let read = if clean { read_hole(&text[s + 2..e - 2], &mut out.vars, &mut typed) } else { Err("A value can't hold quotes or comments.".into()) };
        match read {
            Ok(var) if l == Lang::Shell && at != At::Code => {
                let n = &out.vars[var].name;
                out.err = Some(VarErr { span: s..e, msg: format!("In the Shell a value goes outside quotes: 'text' + {{{{{n}}}}}, or ${{ {{{{{n}}}}} }} in a template.") });
                return out;
            }
            Ok(var) => out.holes.push(Hole { var, span: s..e, at }),
            // In a string, braces that don't read as a placeholder are its text, such as a stored template: only code fails.
            Err(_) if at != At::Code => {}
            Err(msg) => {
                out.err = Some(VarErr { span: s..e, msg });
                return out;
            }
        }
        i = e;
    }
    out
}

/// Reads the inside of one placeholder into `vars`: the index of its value, or why it does not read. `typed` says whether
/// each value's type was written, so two written types that differ are refused in either order.
fn read_hole(inner: &str, vars: &mut Vec<Var>, typed: &mut Vec<bool>) -> Result<usize, String> {
    let inner = inner.trim();
    let (head, default) = match inner.split_once('=') {
        Some((h, v)) => (h.trim(), Some(v.trim().to_string())),
        None => (inner, None),
    };
    let (name, ty) = match head.split_once(':') {
        Some((n, t)) => (n.trim(), Some(t.trim())),
        None => (head, None),
    };
    if !valid_name(name) {
        return Err(format!("\"{name}\" is not a value name: use letters, digits and _."));
    }
    let ty = match ty {
        Some(t) => Some(Ty::read(t).ok_or_else(|| format!("\"{t}\" is not a type. Use {}.", TYPES.map(|t| t.0).join(", ")))?),
        None => None,
    };
    let Some(k) = vars.iter().position(|v| v.name == name) else {
        typed.push(ty.is_some());
        vars.push(Var { name: name.into(), ty: ty.unwrap_or(Ty::Text), default });
        return Ok(vars.len() - 1);
    };
    let v = &mut vars[k];
    match ty {
        Some(t) if typed[k] && v.ty != t => return Err(format!("{name} is already {}.", v.ty.spec())),
        // The first mention without a type is text until a later one names it.
        Some(t) => {
            v.ty = t;
            typed[k] = true;
        }
        None => {}
    }
    if v.default.is_none() {
        v.default = default;
    }
    Ok(k)
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

fn is_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    let [y, m, d] = p[..] else { return false };
    let (Some(y), Some(m), Some(d)) = (digits(y, 4), digits(m, 2), digits(d, 2)) else { return false };
    (1..=12).contains(&m) && d >= 1 && i64::from(d) <= crate::db::dates::month_len(i64::from(y), i64::from(m))
}

fn is_datetime(s: &str) -> bool {
    let Some((date, time)) = s.split_once([' ', 'T']) else { return false };
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (time, None),
    };
    let ok = |v: &str, max: u32| digits(v, 2).is_some_and(|v| v <= max);
    let time_ok = match hms.split(':').collect::<Vec<_>>()[..] {
        [h, m] => frac.is_none() && ok(h, 23) && ok(m, 59),
        [h, m, sec] => ok(h, 23) && ok(m, 59) && ok(sec, 59),
        _ => false,
    };
    is_date(date) && time_ok && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

fn int(s: &str) -> Option<String> {
    // Unsigned too, for BIGINT UNSIGNED ids past i64.
    s.parse::<i64>().map(|v| v.to_string()).or_else(|_| s.parse::<u64>().map(|v| v.to_string())).ok()
}

/// A negative number in parentheses: after a minus in the statement, a bare one would make `--`, a comment.
fn signed(n: String) -> String {
    if n.starts_with('-') { format!("({n})") } else { n }
}

pub fn number(s: &str) -> Option<String> {
    // Only plain decimals: Rust also reads inf and NaN, which no server takes as a number.
    let ok = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E')) && s.parse::<f64>().is_ok_and(f64::is_finite);
    ok.then(|| s.to_string())
}

/// `bs` is whether a MySQL server reads backslashes as escapes, false under NO_BACKSLASH_ESCAPES. Quotes are doubled, not
/// escaped, so the literal ends where it should either way; `bs` keeps a backslash in the value as one backslash.
fn quote(s: &str, d: Dialect, bs: bool) -> String {
    if d == Dialect::Postgres { pg_lit(s) } else { lit(s, bs) }
}

/// The value once it reads as its type, before any quoting, or why it can't.
fn checked(v: &Var, value: &str) -> Result<String, String> {
    let s = value.trim();
    let bad = |what: &str| format!("{} needs {what}.", v.name);
    match &v.ty {
        Ty::Text => Ok(value.to_string()),
        Ty::Int => int(s).ok_or_else(|| bad("a whole number")),
        Ty::Number => number(s).ok_or_else(|| bad("a number")),
        Ty::Date if is_date(s) => Ok(s.into()),
        Ty::Date => Err(bad("a date like 2026-11-03")),
        Ty::DateTime if is_datetime(s) => Ok(s.into()),
        Ty::DateTime => Err(bad("a date and time like 2026-11-03 14:30")),
        Ty::IntList => {
            let parts: Vec<&str> = s.split([',', ' ', '\n', '\t', ';']).filter(|p| !p.is_empty()).collect();
            let ints: Option<Vec<String>> = parts.iter().map(|p| int(p)).collect();
            ints.filter(|i| !i.is_empty()).map(|i| i.join(", ")).ok_or_else(|| bad("whole numbers separated by commas"))
        }
        Ty::Ident if s.is_empty() || s.contains('\0') => Err(bad("a name")),
        Ty::Ident => Ok(s.into()),
        Ty::Choice(c) if c.iter().any(|c| c == s) => Ok(s.into()),
        Ty::Choice(c) => Err(bad(&format!("one of {}", c.join(", ")))),
    }
}

/// A JavaScript and JSON string that no value can end, wherever it lands: quotes, backticks, `$`, `/` and line
/// breaks are all escapes, so even text read as code by mistake stays inside the string, template, regex or comment
/// around it.
fn js_str(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '"' | '\'' | '`' | '$' | '/' | '\u{2028}' | '\u{2029}' => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c if c.is_control() => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A checked value as a JavaScript literal. Dates are ISODate with the time and a Z always written: the Shell's ISODate
/// is `new Date`, which reads a time without a zone as local, where the Documents bar reads it as UTC.
fn js_literal(ty: &Ty, c: String, shell: bool) -> String {
    match ty {
        // `a -{{n}}` would make `--` in JavaScript; JSON5 takes no parentheses and has no such operator.
        Ty::Int | Ty::Number if shell => signed(c),
        Ty::Int | Ty::Number => c,
        Ty::IntList => format!("[{c}]"),
        Ty::Date => format!("ISODate(\"{c}T00:00:00Z\")"),
        Ty::DateTime => {
            let t = c.replacen(' ', "T", 1);
            let secs = if t.matches(':').count() == 1 { ":00" } else { "" };
            format!("ISODate(\"{t}{secs}Z\")")
        }
        _ => js_str(&c),
    }
}

/// The literal one value becomes, or why it can't.
fn literal(v: &Var, value: &str, l: Lang, bs: bool) -> Result<String, String> {
    let c = checked(v, value)?;
    let Lang::Sql(d) = l else { return Ok(js_literal(&v.ty, c, l == Lang::Shell)) };
    Ok(match &v.ty {
        Ty::Int | Ty::Number => signed(c),
        Ty::IntList => c.split(", ").map(|n| signed(n.into())).collect::<Vec<_>>().join(", "),
        Ty::Ident => c.split('.').map(|p| d.quote_ident(p)).collect::<Vec<_>>().join("."),
        _ => quote(&c, d, bs),
    })
}

/// The value written inside a string that the statement already quotes, so it can't end that string. `bs` is the MySQL
/// backslash reading, see `quote`.
fn in_string(v: &Var, value: &str, at: At, l: Lang, bs: bool) -> Result<String, String> {
    let Lang::Sql(d) = l else {
        if at == At::Code {
            return literal(v, value, l, bs);
        }
        // Inside a query string: escaped as a JSON string, which has no quote left to end it, without its own quotes.
        let s = js_str(&checked(v, value)?);
        return Ok(s[1..s.len() - 1].to_string());
    };
    let c = checked(v, value)?;
    let (q, escapes) = match at {
        At::Str(q) if d == Dialect::Postgres => {
            // A plain PostgreSQL string reads backslashes as escapes only under standard_conforming_strings off, which
            // the statement can't tell, so a backslash has no safe spelling here.
            if c.contains('\\') {
                return Err(format!("{} can't hold a backslash inside this string: write E'...' instead.", v.name));
            }
            (q, false)
        }
        At::Str(q) => (q, bs),
        At::EStr => ('\'', true),
        At::Code => return literal(v, value, l, bs),
    };
    let c = match escapes {
        true if d == Dialect::Postgres => c.replace('\\', "\\\\"),
        true => mysql_escapes(&c),
        false => c,
    };
    Ok(c.replace(q, &format!("{q}{q}")))
}

/// The values of a text by name: an empty one takes the default, and is refused when there is none.
pub type Values = std::collections::HashMap<String, String>;

/// The text with every placeholder replaced by the literal of its value. `bs` is the MySQL backslash reading, see `quote`.
pub fn fill(text: &str, p: &Parsed, values: &Values, l: impl Into<Lang>, bs: bool) -> Result<String, String> {
    let l = l.into();
    if let Some(e) = &p.err {
        return Err(e.msg.clone());
    }
    // An empty value with no default is refused, not sent as '': braces of a stored template would otherwise vanish.
    // `{{name=}}` allows an empty one.
    let value = |v: &Var| {
        let given = values.get(&v.name).map(String::as_str).unwrap_or("");
        match &v.default {
            _ if !given.trim().is_empty() => Ok(given.to_string()),
            Some(d) => Ok(d.clone()),
            None => Err(format!("{} needs a value.", v.name)),
        }
    };
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for h in &p.holes {
        let v = &p.vars[h.var];
        out.push_str(&text[last..h.span.start]);
        out.push_str(&in_string(v, &value(v)?, h.at, l, bs)?);
        last = h.span.end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// `text`, all or part of a script whose values `full` read, filled with the values by name. Each placeholder takes the
/// type the whole script gives its name, so a statement that only says `{{day}}` still gets a date.
pub fn fill_named(text: &str, full: &Parsed, values: &Values, l: impl Into<Lang>, bs: bool) -> Result<String, String> {
    let l = l.into();
    // Only the text that runs must read: a placeholder still being typed in another statement does not stop this one.
    let mut p = parse(text, l);
    if !p.is_snippet() {
        return Ok(text.to_string());
    }
    // A selection that cuts a placeholder in half keeps its error, so a raw {{ never reaches the server.
    for v in &mut p.vars {
        if let Some(f) = full.vars.iter().find(|f| f.name == v.name) {
            *v = f.clone();
        }
    }
    fill(text, &p, values, l, bs)
}

/// The values of several texts read as one, such as a filter, its sort and its projection: by name, in order, with the
/// first error.
pub fn merge(parts: impl IntoIterator<Item = Parsed>) -> Parsed {
    let mut out = Parsed::default();
    for p in parts {
        for v in p.vars {
            if !out.vars.iter().any(|o| o.name == v.name) {
                out.vars.push(v);
            }
        }
        out.err = out.err.or(p.err);
        out.holes.extend(p.holes);
    }
    out
}

/// The text a language server reads: each placeholder swapped for text of its length, `null` in code and `x` in a
/// string, so positions still match and `{{day:date}}` is not a syntax error.
pub fn mask(text: &str, l: impl Into<Lang>) -> String {
    let mut out = text.to_string();
    for h in parse(text, l).holes.iter().rev() {
        let n = h.span.len();
        let with = if h.at == At::Code { format!("{:<n$}", "null") } else { "x".repeat(n) };
        out.replace_range(h.span.clone(), &with);
    }
    out
}

/// The placeholder that replaces a literal: its type read from the literal and the literal kept as the default when it
/// can be written there.
pub fn infer(token: &str) -> (Ty, Option<String>) {
    let t = token.trim();
    // A Mongo date: date or datetime, kept as it would be typed.
    if let Some(inner) = t.strip_prefix("ISODate(").and_then(|r| r.strip_suffix(')')) {
        let (ty, v) = infer(inner);
        let v = v.map(|v| v.trim_end_matches('Z').trim_end_matches("T00:00:00").replacen('T', " ", 1));
        return match v {
            Some(v) if is_date(&v) => (Ty::Date, Some(v)),
            Some(v) if is_datetime(&v) => (Ty::DateTime, Some(v)),
            _ => (ty, None),
        };
    }
    let quoted = t.strip_prefix(['E', 'e']).filter(|r| r.starts_with('\'')).unwrap_or(t);
    let inner = quoted.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')).or_else(|| quoted.strip_prefix('"').and_then(|r| r.strip_suffix('"')));
    if let Some(inner) = inner {
        let v = inner.replace("''", "'");
        let ty = if is_date(&v) { Ty::Date } else if is_datetime(&v) || is_datetime(v.trim_end_matches('Z')) { Ty::DateTime } else { Ty::Text };
        // A default can't hold what ends, splits or comments out a placeholder, or a backslash, whose reading depends on
        // the server and would be escaped again when filled.
        // Spaces at its ends would be trimmed when the placeholder is read back, so such a value is not kept either.
        let keep = !v.contains(['\'', '"', '=', '|', '#', '`', ';', '$', '\\', '{', '}']) && !v.contains("--") && !v.contains("/*") && !t.starts_with(['E', 'e']) && v.trim() == v;
        return (ty, keep.then_some(v));
    }
    if int(t).is_some() {
        return (Ty::Int, Some(t.to_string()));
    }
    if number(t).is_some() {
        return (Ty::Number, Some(t.to_string()));
    }
    (Ty::Text, None)
}

/// How a placeholder is written.
pub fn placeholder(name: &str, ty: &Ty, default: Option<&str>) -> String {
    match default {
        Some(d) => format!("{{{{{name}:{}={d}}}}}", ty.spec()),
        None => format!("{{{{{name}:{}}}}}", ty.spec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MY: Dialect = Dialect::MySql;
    const PG: Dialect = Dialect::Postgres;

    /// Values in the order the text names them.
    fn vals_of(p: &Parsed, v: &[&str]) -> Values {
        p.vars.iter().zip(v).map(|(var, s)| (var.name.clone(), s.to_string())).collect()
    }

    #[test]
    fn placeholders_are_read_in_code_and_strings_only() {
        let t = "SELECT '{{a}}', `{{b}}` -- {{c}}\nFROM t WHERE x = {{day:date}} /* {{d}} */ AND y = {{day}} AND z = '\\{{e}}'";
        let p = parse(t, MY);
        assert_eq!(p.err, None);
        assert_eq!(p.vars.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["a", "day"]);
        assert_eq!(p.holes.iter().map(|h| h.at).collect::<Vec<_>>(), [At::Str('\''), At::Code, At::Code]);
        assert_eq!(&t[p.holes[1].span.clone()], "{{day:date}}");
        // PostgreSQL casts and dollar quotes don't get in the way.
        let p = parse("SELECT $$ {{no}} $$, created_at::date = {{d:date=2026-11-03}}", PG);
        assert_eq!(p.vars.len(), 1);
        assert_eq!(p.vars[0].default.as_deref(), Some("2026-11-03"));
        assert!(!parse("SELECT 1", MY).is_snippet());
    }

    #[test]
    fn a_value_inside_a_string_joins_it_and_can_never_end_it() {
        let t = "SELECT * FROM t WHERE a LIKE '%{{name}}%' AND b = \"{{name}}\"";
        let p = parse(t, MY);
        let v = vals_of(&p, &["O'Neil\\"]);
        assert_eq!(fill(t, &p, &v, MY, true).unwrap(), "SELECT * FROM t WHERE a LIKE '%O''Neil\\\\%' AND b = \"O'Neil\\\\\"");
        assert_eq!(fill(t, &p, &v, MY, false).unwrap(), "SELECT * FROM t WHERE a LIKE '%O''Neil\\%' AND b = \"O'Neil\\\"");
        // PostgreSQL: a plain string can't say a backslash both ways, E'..' can.
        let t = "SELECT 1 WHERE a LIKE '%{{n}}%' AND b LIKE E'{{n}}'";
        let p = parse(t, PG);
        assert_eq!(fill(t, &p, &vals_of(&p, &["x'y"]), PG, true).unwrap(), "SELECT 1 WHERE a LIKE '%x''y%' AND b LIKE E'x''y'");
        assert!(fill(t, &p, &vals_of(&p, &["x\\y"]), PG, true).unwrap_err().contains("backslash"));
        // Types still check, and a string that only holds braces is left alone.
        let p = parse("SELECT '{{d:date}}', '{{', '}}'", MY);
        assert_eq!(p.holes.len(), 1);
        assert!(fill("SELECT '{{d:date}}', '{{', '}}'", &p, &vals_of(&p, &["soon"]), MY, true).is_err());
        assert!(parse("SELECT $$ {{no}} $$", PG).holes.is_empty());
        assert!(!parse("INSERT INTO mail VALUES ('Hello {{ user.name }}')", MY).is_snippet(), "a stored template became a broken snippet");
    }

    #[test]
    fn a_bad_placeholder_says_why() {
        assert!(parse("x = {{day:dat}}", MY).err.unwrap().msg.contains("not a type"));
        assert!(parse("x = {{9a}}", MY).err.unwrap().msg.contains("not a value name"));
        assert!(parse("x = {{day", MY).err.is_some());
        assert!(parse("x = {{a:int}} AND {{a:date}}", MY).err.unwrap().msg.contains("already"));
        assert!(parse("x = {{a:text}} AND {{a:int}}", MY).err.unwrap().msg.contains("already text"));
        assert_eq!(parse("x = {{a}} AND {{a:int}}", MY).vars[0].ty, Ty::Int);
        // An empty value is refused unless the placeholder allows one.
        let p = parse("x = {{a}} AND y = {{b=}}", MY);
        assert!(fill("x = {{a}} AND y = {{b=}}", &p, &Values::new(), MY, true).unwrap_err().contains("a needs a value"));
        assert_eq!(fill("x = {{a}} AND y = {{b=}}", &p, &vals_of(&p, &["1"]), MY, true).unwrap(), "x = '1' AND y = ''");
        assert!(parse("x = {{a:a||b}}", MY).err.is_some());
    }

    #[test]
    fn values_become_literals_of_their_type() {
        let t = "SELECT * FROM {{t:identifier}} WHERE d = {{day:date}} AND s = {{s:paid|pending}} AND id IN ({{ids:int list}}) LIMIT {{n:int=100}}";
        let p = parse(t, MY);
        let out = fill(t, &p, &vals_of(&p, &["shop.orders", "2026-11-03", "paid", "4405, 4407 4412", ""]), MY, true).unwrap();
        assert_eq!(out, "SELECT * FROM `shop`.`orders` WHERE d = '2026-11-03' AND s = 'paid' AND id IN (4405, 4407, 4412) LIMIT 100");
        assert!(fill(t, &p, &vals_of(&p, &["x", "2026-02-30", "paid", "1", ""]), MY, true).unwrap_err().contains("day needs a date"));
        assert!(fill(t, &p, &vals_of(&p, &["x", "2026-11-03", "shipped", "1", ""]), MY, true).unwrap_err().contains("one of paid, pending"));
        assert!(fill(t, &p, &vals_of(&p, &["x", "2026-11-03", "paid", "1, a", ""]), MY, true).is_err());
        assert!(fill(t, &p, &vals_of(&p, &["x", "2026-11-03", "paid", "1", "1; DROP"]), MY, true).is_err());
        // A negative number goes in parentheses, so a minus before the placeholder never makes a comment.
        let neg = "SELECT a -{{n:int}} AND b = {{x:number}} AND id IN ({{ids:int list}})";
        assert_eq!(fill(neg, &parse(neg, PG), &vals_of(&parse(neg, PG), &["-5", "-1.5", "-1, 2"]), PG, true).unwrap(), "SELECT a -(-5) AND b = (-1.5) AND id IN ((-1), 2)");
        assert_eq!(literal(&Var { name: "id".into(), ty: Ty::Int, default: None }, "18446744073709551615", MY.into(), true).unwrap(), "18446744073709551615");
        // An empty text value takes its default; `=` with nothing after it allows the empty string.
        let t = "SELECT {{s=paid}}, {{q=}}";
        assert_eq!(fill(t, &parse(t, MY), &vals_of(&parse(t, MY), &["", ""]), MY, true).unwrap(), "SELECT 'paid', ''");
        // Under NO_BACKSLASH_ESCAPES a backslash is written once.
        assert_eq!(fill("SELECT {{q}}", &parse("SELECT {{q}}", MY), &vals_of(&parse("SELECT {{q}}", MY), &["C:\\data"]), MY, false).unwrap(), "SELECT 'C:\\data'");
    }

    #[test]
    fn a_text_value_can_never_end_its_literal() {
        let t = "SELECT {{q}}";
        let p = parse(t, MY);
        let evil = "x' OR 1=1 -- \\";
        assert_eq!(fill(t, &p, &vals_of(&p, &[evil]), MY, true).unwrap(), "SELECT 'x'' OR 1=1 -- \\\\'");
        assert_eq!(fill(t, &p, &vals_of(&p, &[evil]), PG, true).unwrap(), "SELECT E'x'' OR 1=1 -- \\\\'");
        assert_eq!(fill("SELECT {{c:identifier}}", &parse("SELECT {{c:identifier}}", PG), &vals_of(&parse("SELECT {{c:identifier}}", PG), &["a\"b"]), PG, true).unwrap(), "SELECT \"a\"\"b\"");
        // The filled text is one statement whatever the value.
        let out = fill(t, &p, &vals_of(&p, &["'; DELETE FROM t; --"]), MY, true).unwrap();
        assert_eq!(crate::db::sql::lex::split_statements(&out, MY).len(), 1);
    }

    #[test]
    fn numbers_and_datetimes() {
        assert_eq!(literal(&Var { name: "n".into(), ty: Ty::Number, default: None }, " 12.5 ", MY.into(), true).unwrap(), "12.5");
        assert!(literal(&Var { name: "n".into(), ty: Ty::Number, default: None }, "inf", MY.into(), true).is_err());
        let dt = Var { name: "at".into(), ty: Ty::DateTime, default: None };
        assert_eq!(literal(&dt, "2026-11-03 14:30", PG.into(), true).unwrap(), "'2026-11-03 14:30'");
        assert_eq!(literal(&dt, "2026-11-03T14:30:05.123", PG.into(), true).unwrap(), "'2026-11-03T14:30:05.123'");
        assert!(literal(&dt, "2026-11-03 25:00", PG.into(), true).is_err());
    }

    #[test]
    fn format_keeps_placeholders_as_written() {
        for d in [MY, PG] {
            let f = crate::db::sql::format::format_script("select id from orders where status = {{status:paid|pending}} and id in ({{ids:int list}}) limit {{n:int=10}}", d);
            assert_eq!(f.formatted, 1);
            for ph in ["{{status:paid|pending}}", "{{ids:int list}}", "{{n:int=10}}"] {
                assert!(f.text.contains(ph), "{ph} lost in {}", f.text);
            }
        }
    }

    #[test]
    fn a_statement_alone_takes_the_types_of_its_script() {
        let full = parse("SELECT {{day:date}};\nSELECT {{day}}", MY);
        let vals = std::collections::HashMap::from([("day".to_string(), "2026-11-03".to_string())]);
        assert_eq!(fill_named("SELECT {{day}}", &full, &vals, MY, true).unwrap(), "SELECT '2026-11-03'");
        let bad = std::collections::HashMap::from([("day".to_string(), "soon".to_string())]);
        assert!(fill_named("SELECT {{day}}", &full, &bad, MY, true).is_err());
        assert_eq!(fill_named("SELECT 1", &full, &vals, MY, true).unwrap(), "SELECT 1");
    }

    #[test]
    fn a_literal_turns_into_a_placeholder_and_back() {
        assert_eq!(infer("'2026-11-03'"), (Ty::Date, Some("2026-11-03".into())));
        assert_eq!(infer("100"), (Ty::Int, Some("100".into())));
        assert_eq!(infer("2.5"), (Ty::Number, Some("2.5".into())));
        assert_eq!(infer("'it''s'"), (Ty::Text, None));
        assert_eq!(infer("'paid'"), (Ty::Text, Some("paid".into())));
        for unsafe_default in ["'#ff0000'", "'a;b'", "'a\\\\b'", "E'x'", "'a}'", "'`x`'"] {
            assert_eq!(infer(unsafe_default).1, None, "{unsafe_default}");
        }
        let (ty, def) = infer("'2026-11-03'");
        let ph = placeholder("day", &ty, def.as_deref());
        assert_eq!(ph, "{{day:date=2026-11-03}}");
        let t = format!("SELECT * FROM o WHERE d = {ph}");
        let p = parse(&t, MY);
        assert_eq!(fill(&t, &p, &vals_of(&p, &[""]), MY, true).unwrap(), "SELECT * FROM o WHERE d = '2026-11-03'");
    }

    const SH: Lang = Lang::Shell;
    const Q: Lang = Lang::Query;

    #[test]
    fn shell_values_go_in_code_and_never_in_strings() {
        let t = "// {{a}}\nconst s = '{{b}}', u = `x ${ {{c:int}} } {{d}}`; /* {{e}} */ find({ day: {{day:date}} })";
        let p = parse(t, SH);
        // A placeholder in a string is refused, and the error names it.
        assert!(p.err.as_ref().unwrap().msg.contains("{{b}}"), "{:?}", p.err);
        let t = "// {{a}}\nconst u = `x ${ {{c:int}} } {{ not one }}`; /* {{e}} */ find({ day: {{day:date}} })";
        let p = parse(t, SH);
        assert_eq!(p.err, None);
        assert_eq!(p.vars.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["c", "day"]);
        let out = fill(t, &p, &vals_of(&p, &["-4", "2026-11-03"]), SH, true).unwrap();
        assert_eq!(out, "// {{a}}\nconst u = `x ${ (-4) } {{ not one }}`; /* {{e}} */ find({ day: ISODate(\"2026-11-03T00:00:00Z\") })");
        // A stored template in a string stays text.
        assert!(!parse("print('Hello {{ user.name }}')", SH).is_snippet());
    }

    #[test]
    fn a_misread_line_can_only_refuse_never_inject() {
        // A regex is not read, so its quote opens a string to the end of the line: the value is refused there...
        assert!(parse("const r = /it's/; f({{x}})", SH).err.is_some());
        // ...and the next line reads again.
        let t = "const r = /it's/;\nf({{x}})";
        let p = parse(t, SH);
        assert_eq!(p.err, None);
        // Whatever the value, a Shell literal holds no quote, backtick, $, / or line break that could end what it is in.
        let evil = "a'b\"c`d${e}/f\ng\u{2028}h\\";
        let out = fill(t, &p, &vals_of(&p, &[evil]), SH, true).unwrap();
        let lit = &out["const r = /it's/;\nf(".len()..out.len() - 1];
        assert!(!lit[1..lit.len() - 1].contains(['\'', '"', '`', '$', '/', '\n', '\u{2028}']), "{lit}");
        assert_eq!(serde_json::from_str::<String>(lit).unwrap(), evil);
    }

    #[test]
    fn query_values_fill_code_and_strings_and_parse() {
        let t = "{ status: {{s:paid|shipped}}, name: { $regex: '^{{prefix}}' }, at: { $gte: {{from:datetime}} }, id: { $in: {{ids:int list}} }, n: {{n:int}} }";
        let p = parse(t, Q);
        assert_eq!(p.err, None);
        let out = fill(t, &p, &vals_of(&p, &["paid", "O'Ne\"il", "2026-11-03 14:30", "1, -2", "-5"]), Q, true).unwrap();
        let d = crate::db::parse_doc(&out).unwrap();
        assert_eq!(d.get_str("status").unwrap(), "paid");
        assert_eq!(d.get_document("name").unwrap().get_str("$regex").unwrap(), "^O'Ne\"il");
        assert_eq!(d.get_document("at").unwrap().get_datetime("$gte").unwrap().timestamp_millis(), 1_793_716_200_000);
        assert_eq!(d.get_document("id").unwrap().get_array("$in").unwrap().len(), 2);
        assert_eq!(d.get_i32("n").or_else(|_| d.get_i64("n").map(|n| n as i32)).unwrap(), -5);
        // A date is midnight UTC.
        let t = "{ d: {{d:date}} }";
        let out = fill(t, &parse(t, Q), &vals_of(&parse(t, Q), &["2024-01-02"]), Q, true).unwrap();
        assert_eq!(crate::db::parse_doc(&out).unwrap().get_datetime("d").unwrap().timestamp_millis(), 1_704_153_600_000);
        // Comments hold no values.
        assert!(!parse("{ a: 1 } // {{x}}", Q).is_snippet());
    }

    #[test]
    fn several_parts_read_as_one_and_a_mask_keeps_positions() {
        let m = merge([parse("{ a: {{x:int}} }", Q), parse("{ {{f:identifier}}: 1 }", Q), parse("{ b: {{x}} }", Q)]);
        assert_eq!(m.vars.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["x", "f"]);
        let t = "f({{day:date}}, `${ {{n}} }`)";
        let masked = mask(t, SH);
        assert_eq!(masked.len(), t.len());
        assert_eq!(masked, "f(null        , `${ null  }`)");
    }

    #[test]
    fn mongo_literals_turn_into_placeholders() {
        assert_eq!(infer("\"paid\""), (Ty::Text, Some("paid".into())));
        assert_eq!(infer("ISODate(\"2026-11-03T00:00:00Z\")"), (Ty::Date, Some("2026-11-03".into())));
        assert_eq!(infer("ISODate('2026-11-03T14:30:00Z')"), (Ty::DateTime, Some("2026-11-03 14:30:00".into())));
    }
}
