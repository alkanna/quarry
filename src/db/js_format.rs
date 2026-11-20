//! Format for Shell scripts and query text (filters, pipelines, validators): every line indented by the brackets it sits
//! in, and a line past the width broken at its longest bracket group, one item per line. Only spaces at line starts and
//! line breaks after an opening bracket, after a comma and before a closing bracket change, so the code reads the same:
//! no token is touched, and strings, templates, regexes and comments are left as they are.

/// Two spaces, as the pretty-printed JSON of `ui::editor::INDENT`, which `db` does not reach into.
const UNIT: usize = 2;

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Space,
    Newline,
    /// A string, template, regex or comment: never split, its insides never reindented.
    Opaque,
    /// A `//` comment, which ends its line.
    LineComment,
    Open,
    Close,
    Comma,
    Code,
}

#[derive(Clone, Copy, Debug)]
struct Tok<'a> {
    k: K,
    s: &'a str,
}

/// Words after which a `/` starts a regex rather than a division.
const BEFORE_REGEX: &[&str] = &["return", "typeof", "case", "in", "of", "new", "delete", "void", "throw", "else", "do", "await"];

fn lex(text: &str) -> Vec<Tok<'_>> {
    let b = text.as_bytes();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        let k = match c {
            b'\n' => {
                i += 1;
                K::Newline
            }
            b' ' | b'\t' | b'\r' => {
                while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r') {
                    i += 1;
                }
                K::Space
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(b.len(), |e| i + e);
                K::LineComment
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                K::Opaque
            }
            b'\'' | b'"' => {
                i = quoted(b, i, c);
                K::Opaque
            }
            b'`' => {
                i = template(b, i);
                K::Opaque
            }
            b'/' if regex_can_start(&out) => {
                let mut j = i + 1;
                let mut class = false;
                while j < b.len() && b[j] != b'\n' && (b[j] != b'/' || class) {
                    match b[j] {
                        b'\\' => j += 1,
                        b'[' => class = true,
                        b']' => class = false,
                        _ => {}
                    }
                    j += 1;
                }
                j = (j + 1).min(b.len());
                while j < b.len() && b[j].is_ascii_alphabetic() {
                    j += 1;
                }
                i = j;
                K::Opaque
            }
            c if word(c) => {
                while i < b.len() && word(b[i]) {
                    i += 1;
                }
                K::Code
            }
            c => {
                i += 1;
                match c {
                    b'(' | b'[' | b'{' => K::Open,
                    b')' | b']' | b'}' => K::Close,
                    b',' => K::Comma,
                    _ => K::Code,
                }
            }
        };
        out.push(Tok { k, s: &text[start..i] });
    }
    out
}

/// A byte of a name or number.
fn word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// Past the end of the '...' or "..." string opening at `i`. A JavaScript string ends at its line's end, so one left
/// open while typing does not swallow the lines after it.
fn quoted(b: &[u8], i: usize, q: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() && b[j] != q && b[j] != b'\n' {
        j += if b[j] == b'\\' { 2 } else { 1 };
    }
    if b.get(j) == Some(&b'\n') { j } else { (j + 1).min(b.len()) }
}

/// Past the end of the template starting at `i`, `${ }` parts included, nested templates too.
fn template(b: &[u8], mut i: usize) -> usize {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => return i + 1,
            b'$' if b.get(i + 1) == Some(&b'{') => {
                i += 2;
                let mut depth = 1;
                while i < b.len() && depth > 0 {
                    match b[i] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        b'`' => {
                            i = template(b, i);
                            continue;
                        }
                        b'\'' | b'"' => {
                            i = quoted(b, i, b[i]);
                            continue;
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    b.len()
}

/// Whether a `/` here starts a regex: after nothing, an opening bracket, a comma, an operator or a word like return.
fn regex_can_start(before: &[Tok]) -> bool {
    match before.iter().rev().find(|t| !matches!(t.k, K::Space | K::Newline | K::LineComment)) {
        None => true,
        Some(t) => match t.k {
            K::Open | K::Comma => true,
            K::Close | K::Opaque => false,
            _ => BEFORE_REGEX.contains(&t.s) || !word(t.s.as_bytes()[0]),
        },
    }
}

fn flat(toks: &[Tok]) -> String {
    let s: String = toks.iter().map(|t| t.s).collect();
    s.trim().to_string()
}

/// Whether a line continues the one above it: a member chain or an operator first, after a line that did not end a
/// statement or a block. `++i` and `--i` start statements of their own.
fn continues(line: &[Tok], prev_end: Option<char>) -> bool {
    let s = flat(line);
    let op = [".", "?.", "&&", "||", "??", "?", ":", "+", "*", "|", "&"].iter().any(|p| s.starts_with(p));
    op && !s.starts_with("...") && !s.starts_with("++") && !matches!(prev_end, Some(';' | '{' | '}'))
}

/// Breaks `toks`, one line at `level`, into lines that fit `width` where a bracket group allows it.
fn lay(toks: &[Tok], level: usize, width: usize, out: &mut Vec<String>) {
    let line = flat(toks);
    let pad = " ".repeat(level * UNIT);
    if line.is_empty() {
        return;
    }
    if pad.len() + line.chars().count() <= width || toks.iter().any(|t| t.s.contains('\n')) {
        out.push(pad + &line);
        return;
    }
    // The groups that open and close in this line, at its top level.
    let mut groups = Vec::new();
    let mut open = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        match t.k {
            K::Open => open.push(i),
            K::Close => {
                if let Some(o) = open.pop()
                    && open.is_empty()
                {
                    groups.push((o, i));
                }
            }
            _ => {}
        }
    }
    let inner = |&(o, c): &(usize, usize)| toks[o + 1..c].iter().any(|t| !matches!(t.k, K::Space));
    let Some((o, c)) = groups.into_iter().filter(inner).max_by_key(|&(o, c)| toks[o..=c].iter().map(|t| t.s.len()).sum::<usize>()) else {
        out.push(pad + &line);
        return;
    };
    out.push(pad + &flat(&toks[..=o]));
    // One item per line, each ending with its comma.
    let mut depth = 0;
    let mut from = o + 1;
    for i in o + 1..c {
        match toks[i].k {
            K::Open => depth += 1,
            K::Close => depth -= 1,
            K::Comma if depth == 0 => {
                lay(&toks[from..=i], level + 1, width, out);
                from = i + 1;
            }
            _ => {}
        }
    }
    lay(&toks[from..c], level + 1, width, out);
    lay(&toks[c..], level, width, out);
}

/// `text` formatted, lines no wider than `width` where a bracket group can be broken.
pub fn format(text: &str, width: usize) -> String {
    let toks = lex(text);
    let mut out = Vec::new();
    // The level of the line each open bracket is on.
    let mut stack: Vec<usize> = Vec::new();
    let mut prev_end = None;
    for line in toks.split(|t| t.k == K::Newline) {
        let first = line.iter().position(|t| t.k != K::Space);
        let Some(first) = first else {
            out.push(String::new());
            continue;
        };
        let line = &line[first..];
        // A line that starts by closing goes back to the level of the line its outermost leading closer opened on.
        let leading = line.iter().take_while(|t| t.k == K::Close).count();
        let mut level = stack.last().map_or(0, |l| l + 1);
        for _ in 0..leading {
            if let Some(l) = stack.pop() {
                level = l;
            }
        }
        if leading == 0 && continues(line, prev_end) {
            level += 1;
        }
        prev_end = line.iter().rev().find(|t| !matches!(t.k, K::Space | K::LineComment)).and_then(|t| t.s.chars().last());
        for t in &line[leading..] {
            match t.k {
                K::Open => stack.push(level),
                K::Close => {
                    stack.pop();
                }
                _ => {}
            }
        }
        lay(line, level, width, &mut out);
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same_tokens(a: &str, b: &str) -> bool {
        let t = |s: &str| lex(s).into_iter().filter(|t| !matches!(t.k, K::Space | K::Newline)).map(|t| t.s.to_string()).collect::<Vec<_>>();
        t(a) == t(b)
    }

    #[test]
    fn lines_are_indented_by_their_brackets() {
        let t = "async function run() {\nconst docs = await orders.find({ status: 'paid' })\n.limit(3).toArray();\n  if (docs.length) {\nreturn docs.map(d => {\nreturn d.total;\n});\n} else {\n        return [];\n}\n}";
        let f = format(t, 80);
        assert_eq!(f, "async function run() {\n  const docs = await orders.find({ status: 'paid' })\n    .limit(3).toArray();\n  if (docs.length) {\n    return docs.map(d => {\n      return d.total;\n    });\n  } else {\n    return [];\n  }\n}");
        assert!(same_tokens(t, &f));
        // Formatting twice changes nothing more.
        assert_eq!(format(&f, 80), f);
    }

    #[test]
    fn a_long_line_breaks_at_its_largest_group() {
        let t = "async function run() {\n  return await orders.updateOne(\n  { _id: \"c8c1b2d2\" },\n  { $pull: { \"lines.byKey.be25ed1e-5cb1-4862-b833\": { \"item.uid\": { $in: [46, 47, 48] } } } }\n)\n}";
        let f = format(t, 60);
        assert_eq!(f, "async function run() {\n  return await orders.updateOne(\n    { _id: \"c8c1b2d2\" },\n    {\n      $pull: {\n        \"lines.byKey.be25ed1e-5cb1-4862-b833\": {\n          \"item.uid\": { $in: [46, 47, 48] }\n        }\n      }\n    }\n  )\n}");
        assert!(same_tokens(t, &f));
        let p = "[{ $match: { status: 'paid', total: { $gt: 50 } } }, { $group: { _id: '$customer', n: { $sum: 1 } } }]";
        assert_eq!(format(p, 60), "[\n  { $match: { status: 'paid', total: { $gt: 50 } } },\n  { $group: { _id: '$customer', n: { $sum: 1 } } }\n]");
    }

    #[test]
    fn strings_templates_regexes_and_comments_stay_as_written() {
        let t = "const q = sql`\n    SELECT ( a, b )\n  FROM t`;\nconst r = /[(,]/g, s = '( , )'; // ( a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p )\n/* (\n  , */ x(1)";
        let f = format(t, 30);
        assert!(f.contains("sql`\n    SELECT ( a, b )\n  FROM t`"), "{f}");
        assert!(f.contains("/[(,]/g") && f.contains("'( , )'") && f.contains("/* (\n  , */"), "{f}");
        assert!(same_tokens(t, &f));
        // A string left open ends at its line, and the lines after it are still laid out.
        assert_eq!(format("const a = 'abc\nif (x) {\nf();\n}", 80), "const a = 'abc\nif (x) {\n  f();\n}");
        // A statement that starts with ++ is not a continuation.
        assert_eq!(format("for (;;) {\n++count;\n}", 80), "for (;;) {\n  ++count;\n}");
        // A division is not a regex.
        assert_eq!(format("const a = b / c, d = (e) / 2;", 80), "const a = b / c, d = (e) / 2;");
    }
}
