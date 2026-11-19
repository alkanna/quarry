//! Snippets: SQL scripts with values to fill in, written `{{name}}`, `{{name:type}}` or `{{name:type=default}}` in the
//! statement itself, so nothing about them is stored apart from the script. A placeholder counts only in code: inside a
//! string or a comment it is plain text. Values go into the statement as literals of their type, never as SQL text.
use crate::db::sql::lex::{tokens, Tok};
use crate::db::sql::value::{lit, pg_lit};
use crate::db::sql::Dialect;
use std::ops::Range;

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

/// One placeholder in the text: which value it takes and the bytes it covers, braces included.
#[derive(Clone, Debug, PartialEq)]
pub struct Hole {
    pub var: usize,
    pub span: Range<usize>,
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

/// Whether byte `i` is code, outside strings, quoted names and comments.
fn code_mask(text: &str, d: Dialect) -> Vec<bool> {
    let mut m = vec![false; text.len()];
    for (t, r) in tokens(text, d) {
        if matches!(t, Tok::Word | Tok::Punct | Tok::Space | Tok::ExecOpen | Tok::ExecClose) {
            m[r].fill(true);
        }
    }
    m
}

fn valid_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn parse(text: &str, d: Dialect) -> Parsed {
    let mut out = Parsed::default();
    if !text.contains("{{") {
        return out;
    }
    let code = code_mask(text, d);
    let mut i = 0;
    while let Some(off) = text[i..].find("{{") {
        let s = i + off;
        if !code[s] || !code[s + 1] {
            i = s + 2;
            continue;
        }
        let Some(len) = text[s + 2..].find("}}") else {
            out.err = Some(VarErr { span: s..text.len(), msg: "This {{ has no closing }}.".into() });
            return out;
        };
        let e = s + 2 + len + 2;
        let span = s..e;
        let fail = |msg: String| Some(VarErr { span: span.clone(), msg });
        if !code[s..e].iter().all(|c| *c) {
            out.err = fail("A value can't hold quotes or comments.".into());
            return out;
        }
        let inner = text[s + 2..e - 2].trim();
        let (head, default) = match inner.split_once('=') {
            Some((h, v)) => (h.trim(), Some(v.trim().to_string())),
            None => (inner, None),
        };
        let (name, ty) = match head.split_once(':') {
            Some((n, t)) => (n.trim(), Some(t.trim())),
            None => (head, None),
        };
        if !valid_name(name) {
            out.err = fail(format!("\"{name}\" is not a value name: use letters, digits and _."));
            return out;
        }
        let ty = match ty.map(|t| (t, Ty::read(t))) {
            Some((t, None)) => {
                out.err = fail(format!("\"{t}\" is not a type. Use text, int, number, date, datetime, int list, identifier or a|b|c."));
                return out;
            }
            Some((_, Some(t))) => Some(t),
            None => None,
        };
        let var = match out.vars.iter().position(|v| v.name == name) {
            Some(k) => {
                let v = &mut out.vars[k];
                match ty {
                    Some(t) if v.ty != t && v.ty != Ty::Text => {
                        out.err = fail(format!("{name} is already a {}.", v.ty.spec()));
                        return out;
                    }
                    // The first mention without a type is text until a later one names it.
                    Some(t) => v.ty = t,
                    None => {}
                }
                if v.default.is_none() {
                    v.default = default;
                }
                k
            }
            None => {
                out.vars.push(Var { name: name.into(), ty: ty.unwrap_or(Ty::Text), default });
                out.vars.len() - 1
            }
        };
        out.holes.push(Hole { var, span });
        i = e;
    }
    out
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

fn is_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    let [y, m, d] = p[..] else { return false };
    let (Some(y), Some(m), Some(d)) = (digits(y, 4), digits(m, 2), digits(d, 2)) else { return false };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    (1..=12).contains(&m) && d >= 1 && d <= days[m as usize - 1]
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
    s.parse::<i64>().ok().map(|v| v.to_string())
}

fn number(s: &str) -> Option<String> {
    // Only plain decimals: Rust also reads inf and NaN, which no server takes as a number.
    let ok = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E')) && s.parse::<f64>().is_ok_and(f64::is_finite);
    ok.then(|| s.to_string())
}

fn quote(s: &str, d: Dialect) -> String {
    // MySQL's backslash escapes are on by default. Written for them, a value still ends where it should under
    // NO_BACKSLASH_ESCAPES, since quotes are doubled, not escaped: at worst a backslash reads twice.
    if d == Dialect::Postgres { pg_lit(s) } else { lit(s, true) }
}

/// The literal one value becomes, or why it can't.
pub fn literal(v: &Var, value: &str, d: Dialect) -> Result<String, String> {
    let s = value.trim();
    let bad = |what: &str| Err(format!("{} needs {what}.", v.name));
    match &v.ty {
        Ty::Text => Ok(quote(value, d)),
        Ty::Int => int(s).ok_or(()).or_else(|_| bad("a whole number")),
        Ty::Number => number(s).ok_or(()).or_else(|_| bad("a number")),
        Ty::Date if is_date(s) => Ok(quote(s, d)),
        Ty::Date => bad("a date like 2026-11-03"),
        Ty::DateTime if is_datetime(s) => Ok(quote(s, d)),
        Ty::DateTime => bad("a date and time like 2026-11-03 14:30"),
        Ty::IntList => {
            let parts: Vec<&str> = s.split([',', ' ', '\n', '\t', ';']).filter(|p| !p.is_empty()).collect();
            let ints: Option<Vec<String>> = parts.iter().map(|p| int(p)).collect();
            match ints {
                Some(i) if !i.is_empty() => Ok(i.join(", ")),
                _ => bad("whole numbers separated by commas"),
            }
        }
        Ty::Ident if s.is_empty() || s.contains('\0') => bad("a name"),
        Ty::Ident => Ok(s.split('.').map(|p| d.quote_ident(p)).collect::<Vec<_>>().join(".")),
        Ty::Choice(c) if c.iter().any(|c| c == s) => Ok(quote(s, d)),
        Ty::Choice(c) => bad(&format!("one of {}", c.join(", "))),
    }
}

/// The text with every placeholder replaced by the literal of its value. `values` follow `p.vars`; an empty one takes
/// the default when there is one.
pub fn fill(text: &str, p: &Parsed, values: &[String], d: Dialect) -> Result<String, String> {
    if let Some(e) = &p.err {
        return Err(e.msg.clone());
    }
    let lits: Vec<String> = p
        .vars
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let given = values.get(i).map(String::as_str).unwrap_or("");
            let value = if given.trim().is_empty() && v.ty != Ty::Text { v.default.as_deref().unwrap_or("") } else { given };
            literal(v, value, d)
        })
        .collect::<Result<_, _>>()?;
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for h in &p.holes {
        out.push_str(&text[last..h.span.start]);
        out.push_str(&lits[h.var]);
        last = h.span.end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// `text`, all or part of a script whose values `full` read, filled with the values by name. Each placeholder takes the
/// type the whole script gives its name, so a statement that only says `{{day}}` still gets a date.
pub fn fill_named(text: &str, full: &Parsed, values: &std::collections::HashMap<String, String>, d: Dialect) -> Result<String, String> {
    if let Some(e) = &full.err {
        return Err(e.msg.clone());
    }
    let mut p = parse(text, d);
    if !p.is_snippet() {
        return Ok(text.to_string());
    }
    p.err = None;
    for v in &mut p.vars {
        if let Some(f) = full.vars.iter().find(|f| f.name == v.name) {
            *v = f.clone();
        }
    }
    let vals: Vec<String> = p.vars.iter().map(|v| values.get(&v.name).cloned().unwrap_or_default()).collect();
    fill(text, &p, &vals, d)
}

/// The placeholder that replaces a literal: its type read from the literal and the literal kept as the default when it
/// can be written there.
pub fn infer(token: &str) -> (Ty, Option<String>) {
    let t = token.trim();
    let quoted = t.strip_prefix(['E', 'e']).filter(|r| r.starts_with('\'')).unwrap_or(t);
    if let Some(inner) = quoted.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        let v = inner.replace("''", "'");
        let ty = if is_date(&v) { Ty::Date } else if is_datetime(&v) { Ty::DateTime } else { Ty::Text };
        // A default can't hold what ends or splits a placeholder.
        let keep = !v.contains(['\'', '"', '=', '|']) && !v.contains("}}") && !v.contains("--") && !v.contains("/*");
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

    fn vals(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn placeholders_are_read_only_in_code() {
        let t = "SELECT '{{a}}', `{{b}}` -- {{c}}\nFROM t WHERE x = {{day:date}} /* {{d}} */ AND y = {{day}}";
        let p = parse(t, MY);
        assert_eq!(p.err, None);
        assert_eq!(p.vars, vec![Var { name: "day".into(), ty: Ty::Date, default: None }]);
        assert_eq!(p.holes.len(), 2);
        assert_eq!(&t[p.holes[0].span.clone()], "{{day:date}}");
        // PostgreSQL casts and dollar quotes don't get in the way.
        let p = parse("SELECT $$ {{no}} $$, created_at::date = {{d:date=2026-11-03}}", PG);
        assert_eq!(p.vars.len(), 1);
        assert_eq!(p.vars[0].default.as_deref(), Some("2026-11-03"));
        assert!(!parse("SELECT 1", MY).is_snippet());
    }

    #[test]
    fn a_bad_placeholder_says_why() {
        assert!(parse("x = {{day:dat}}", MY).err.unwrap().msg.contains("not a type"));
        assert!(parse("x = {{9a}}", MY).err.unwrap().msg.contains("not a value name"));
        assert!(parse("x = {{day", MY).err.is_some());
        assert!(parse("x = {{a:int}} AND {{a:date}}", MY).err.unwrap().msg.contains("already"));
        assert!(parse("x = {{a:a||b}}", MY).err.is_some());
    }

    #[test]
    fn values_become_literals_of_their_type() {
        let t = "SELECT * FROM {{t:identifier}} WHERE d = {{day:date}} AND s = {{s:paid|pending}} AND id IN ({{ids:int list}}) LIMIT {{n:int=100}}";
        let p = parse(t, MY);
        let out = fill(t, &p, &vals(&["shop.orders", "2026-11-03", "paid", "4405, 4407 4412", ""]), MY).unwrap();
        assert_eq!(out, "SELECT * FROM `shop`.`orders` WHERE d = '2026-11-03' AND s = 'paid' AND id IN (4405, 4407, 4412) LIMIT 100");
        assert!(fill(t, &p, &vals(&["x", "2026-02-30", "paid", "1", ""]), MY).unwrap_err().contains("day needs a date"));
        assert!(fill(t, &p, &vals(&["x", "2026-11-03", "shipped", "1", ""]), MY).unwrap_err().contains("one of paid, pending"));
        assert!(fill(t, &p, &vals(&["x", "2026-11-03", "paid", "1, a", ""]), MY).is_err());
        assert!(fill(t, &p, &vals(&["x", "2026-11-03", "paid", "1", "1; DROP"]), MY).is_err());
    }

    #[test]
    fn a_text_value_can_never_end_its_literal() {
        let t = "SELECT {{q}}";
        let p = parse(t, MY);
        let evil = "x' OR 1=1 -- \\";
        assert_eq!(fill(t, &p, &vals(&[evil]), MY).unwrap(), "SELECT 'x'' OR 1=1 -- \\\\'");
        assert_eq!(fill(t, &p, &vals(&[evil]), PG).unwrap(), "SELECT E'x'' OR 1=1 -- \\\\'");
        assert_eq!(fill("SELECT {{c:identifier}}", &parse("SELECT {{c:identifier}}", PG), &vals(&["a\"b"]), PG).unwrap(), "SELECT \"a\"\"b\"");
        // The filled text is one statement whatever the value.
        let out = fill(t, &p, &vals(&["'; DELETE FROM t; --"]), MY).unwrap();
        assert_eq!(crate::db::sql::lex::split_statements(&out, MY).len(), 1);
    }

    #[test]
    fn numbers_and_datetimes() {
        assert_eq!(literal(&Var { name: "n".into(), ty: Ty::Number, default: None }, " 12.5 ", MY).unwrap(), "12.5");
        assert!(literal(&Var { name: "n".into(), ty: Ty::Number, default: None }, "inf", MY).is_err());
        let dt = Var { name: "at".into(), ty: Ty::DateTime, default: None };
        assert_eq!(literal(&dt, "2026-11-03 14:30", PG).unwrap(), "'2026-11-03 14:30'");
        assert_eq!(literal(&dt, "2026-11-03T14:30:05.123", PG).unwrap(), "'2026-11-03T14:30:05.123'");
        assert!(literal(&dt, "2026-11-03 25:00", PG).is_err());
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
        assert_eq!(fill_named("SELECT {{day}}", &full, &vals, MY).unwrap(), "SELECT '2026-11-03'");
        let bad = std::collections::HashMap::from([("day".to_string(), "soon".to_string())]);
        assert!(fill_named("SELECT {{day}}", &full, &bad, MY).is_err());
        assert_eq!(fill_named("SELECT 1", &full, &vals, MY).unwrap(), "SELECT 1");
    }

    #[test]
    fn a_literal_turns_into_a_placeholder_and_back() {
        assert_eq!(infer("'2026-11-03'"), (Ty::Date, Some("2026-11-03".into())));
        assert_eq!(infer("100"), (Ty::Int, Some("100".into())));
        assert_eq!(infer("2.5"), (Ty::Number, Some("2.5".into())));
        assert_eq!(infer("'it''s'"), (Ty::Text, None));
        assert_eq!(infer("'paid'"), (Ty::Text, Some("paid".into())));
        let (ty, def) = infer("'2026-11-03'");
        let ph = placeholder("day", &ty, def.as_deref());
        assert_eq!(ph, "{{day:date=2026-11-03}}");
        let t = format!("SELECT * FROM o WHERE d = {ph}");
        let p = parse(&t, MY);
        assert_eq!(fill(&t, &p, &vals(&[""]), MY).unwrap(), "SELECT * FROM o WHERE d = '2026-11-03'");
    }
}
