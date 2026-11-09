//! Shortens the type text of signature and hover cards, the way IDEs elide long types: big unions
//! keep their first members, nested object types become `{ … }` and long generic arguments `<…>`.
//! Pure text in and out, so the rules are tested without a window.

use std::ops::Range;

/// Union members kept before `| … N more`; shorter unions stay whole.
const UNION_KEEP: usize = 3;
const UNION_MAX: usize = 4;
/// An object type longer than this, or holding another object type, becomes `{ … }`.
const OBJECT_MAX: usize = 60;
const GENERIC_MAX: usize = 40;
/// Marks the active parameter while the text is shortened, so it is never cut away.
const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';

fn has_mark(s: &str) -> bool {
    s.contains(OPEN) || s.contains(CLOSE)
}

/// The index after the string literal that starts at `i`, which holds a quote.
fn skip_string(b: &[char], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            '\\' => j += 2,
            c if c == q => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// The index of the closer that matches the opener at `i`, if there is one.
fn matching(b: &[char], i: usize) -> Option<usize> {
    let mut depth = 0;
    let mut j = i;
    while j < b.len() {
        match b[j] {
            '"' | '\'' | '`' => {
                j = skip_string(b, j);
                continue;
            }
            '{' | '(' | '[' => depth += 1,
            '<' if j > 0 && (b[j - 1].is_alphanumeric() || b[j - 1] == '_' || b[j - 1] == '$') => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            // The arrow of a function type is not a closing bracket.
            '>' if j > 0 && b[j - 1] != '=' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Splits at the characters of `seps` that sit outside brackets and strings, keeping the pieces
/// and the separators that ended them.
fn split_top(b: &[char], seps: &[char]) -> Vec<(Vec<char>, Option<char>)> {
    let mut out = Vec::new();
    let (mut cur, mut i) = (Vec::new(), 0);
    while i < b.len() {
        let c = b[i];
        if matches!(c, '"' | '\'' | '`') {
            let e = skip_string(b, i);
            cur.extend(&b[i..e]);
            i = e;
        } else if matches!(c, '{' | '(' | '[') || (c == '<' && i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_' || b[i - 1] == '$')) {
            let e = matching(b, i).map_or(b.len(), |e| e + 1);
            cur.extend(&b[i..e]);
            i = e;
        } else if seps.contains(&c) {
            out.push((std::mem::take(&mut cur), Some(c)));
            i += 1;
        } else {
            cur.push(c);
            i += 1;
        }
    }
    out.push((cur, None));
    out
}

/// One member of a union or one parameter: its brackets are shortened by the rules.
fn member(b: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if matches!(c, '"' | '\'' | '`') {
            let e = skip_string(b, i);
            out.extend(&b[i..e]);
            i = e;
            continue;
        }
        let generic = c == '<' && i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_' || b[i - 1] == '$');
        let Some(e) = (matches!(c, '{' | '(' | '[') || generic).then(|| matching(b, i)).flatten() else {
            out.push(c);
            i += 1;
            continue;
        };
        let inner = list(&b[i + 1..e]);
        let marked = has_mark(&inner);
        match c {
            '{' if !marked && (inner.chars().count() > OBJECT_MAX || inner.contains('{')) => out.push_str("{ … }"),
            '<' if !marked && inner.chars().count() > GENERIC_MAX => out.push_str("<…>"),
            _ => {
                out.push(c);
                out.push_str(&inner);
                out.push(b[e]);
            }
        }
        i = e + 1;
    }
    out
}

/// Text between brackets: split at commas and semicolons so a long union is cut only inside its
/// own entry, then each entry's unions and brackets are shortened.
fn list(b: &[char]) -> String {
    let mut out = String::new();
    for (seg, sep) in split_top(b, &[',', ';']) {
        let members = split_top(&seg, &['|']);
        let text = |m: &[(Vec<char>, Option<char>)]| m.iter().map(|(x, _)| member(x)).collect::<Vec<_>>().join("|");
        let kept: String = if members.len() > UNION_MAX && !members[UNION_KEEP..].iter().any(|(m, _)| m.iter().any(|c| *c == OPEN || *c == CLOSE)) {
            format!("{} | … {} more", text(&members[..UNION_KEEP]).trim_end(), members.len() - UNION_KEEP)
        } else {
            text(&members)
        };
        out.push_str(&kept);
        out.extend(sep);
    }
    out
}

/// `text` with long unions, nested object types and long generic arguments elided, then cut with
/// `…` past `budget` characters.
#[cfg(test)]
pub(crate) fn elide_type(text: &str, budget: usize) -> String {
    elide_sig(text, None, budget).0
}

/// The same for a signature, where `active` (a byte range) is the parameter to keep readable.
/// Returns the shortened text and where that parameter now is.
pub(crate) fn elide_sig(sig: &str, active: Option<&Range<usize>>, budget: usize) -> (String, Option<Range<usize>>) {
    let mut marked = String::with_capacity(sig.len() + 8);
    match active.filter(|a| a.start <= a.end && a.end <= sig.len() && sig.is_char_boundary(a.start) && sig.is_char_boundary(a.end)) {
        Some(a) => {
            marked.push_str(&sig[..a.start]);
            marked.push(OPEN);
            marked.push_str(&sig[a.clone()]);
            marked.push(CLOSE);
            marked.push_str(&sig[a.end..]);
        }
        None => marked.push_str(sig),
    }
    let chars: Vec<char> = marked.chars().collect();
    let mut short: Vec<char> = list(&chars).chars().collect();
    // Past the budget the tail is cut, but never inside the active parameter.
    if short.len() > budget {
        let keep = short.iter().position(|c| *c == CLOSE).map_or(0, |p| p + 1);
        let cut = budget.max(keep);
        if cut < short.len() {
            short.truncate(cut);
            short.push('…');
        }
    }
    let (mut out, mut range) = (String::new(), None::<Range<usize>>);
    for c in short {
        match c {
            OPEN => range = Some(out.len()..out.len()),
            CLOSE => {
                if let Some(r) = &mut range {
                    r.end = out.len();
                }
            }
            c => out.push(c),
        }
    }
    (out, range)
}

/// The first paragraph of markdown docs and whether more follows: it ends at a blank line or at
/// the line of a `@tag`, and a code fence is never cut in half.
pub(crate) fn first_paragraph(doc: &str) -> (&str, bool) {
    let (mut end, mut fence, mut seen) = (doc.len(), false, false);
    let mut at = 0;
    for line in doc.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("```") {
            fence = !fence;
        } else if !fence && seen && (t.is_empty() || t.trim_start_matches('*').starts_with('@')) {
            end = at;
            break;
        }
        seen |= !t.is_empty();
        at += line.len();
    }
    let head = doc[..end].trim_end();
    (head, !doc[end..].trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn union(n: usize) -> String {
        (0..n).map(|i| format!("\"65f0c1a2b3c4d5e6f7a8b9{i:02}\"")).collect::<Vec<_>>().join(" | ")
    }

    #[test]
    fn short_signatures_are_unchanged() {
        let s = "(method) find(filter?: Filter<Doc>, options?: FindOptions): Cursor<Doc>";
        assert_eq!(elide_type(s, 160), s);
        let (t, r) = elide_sig("add(a: number, b: number): number", Some(&(4..13)), 160);
        assert_eq!((t.as_str(), r), ("add(a: number, b: number): number", Some(4..13)));
    }

    #[test]
    fn a_union_of_ninety_literals_keeps_three() {
        let u = union(90);
        let t = elide_type(&u, 400);
        assert!(t.starts_with("\"65f0c1a2b3c4d5e6f7a8b900\" | \"65f0c1a2b3c4d5e6f7a8b901\" | \"65f0c1a2b3c4d5e6f7a8b902\" | … 87 more"), "{t}");
        assert!(!t.contains("b903"));
        assert_eq!(elide_type(&union(4), 400), union(4));
    }

    #[test]
    fn the_screenshot_signature_fits_two_lines() {
        let obj = format!("{{ _id: {}; name: string; age?: number }}", union(90));
        let sig = format!("map(callbackfn: (value: WithId<{obj}>, index: number, array: WithId<{obj}>[]) => any, thisArg?: any): any[]");
        let start = sig.find("callbackfn").unwrap();
        let end = sig.find(", thisArg").unwrap();
        let (t, r) = elide_sig(&sig, Some(&(start..end)), 160);
        assert_eq!(t, "map(callbackfn: (value: WithId<{ … }>, index: number, array: WithId<{ … }>[]) => any, thisArg?: any): any[]");
        let r = r.unwrap();
        assert!(t[r.clone()].starts_with("callbackfn: (value") && t[r].ends_with("=> any"));
    }

    #[test]
    fn nested_objects_and_generics() {
        assert_eq!(elide_type("f(a: { x: { y: number } }): void", 160), "f(a: { … }): void");
        assert_eq!(elide_type("f(a: { x: number; y: string }): void", 160), "f(a: { x: number; y: string }): void");
        let long = "Promise<Map<string, Array<{ id: number; name: string; tags: string[] }>>>";
        assert_eq!(elide_type(&format!("f(): {long}"), 160), "f(): Promise<Map<string, Array<…>>>");
        assert_eq!(elide_type("f(): Promise<Record<string, SomeVeryLongTypeName<WithAnotherLongArgument, AndAThird>>>", 160), "f(): Promise<Record<…>>");
        assert_eq!(elide_type("f(): Array<number>", 160), "f(): Array<number>");
    }

    #[test]
    fn arrow_types_and_strings_are_respected() {
        let s = "on(cb: (err: Error | null, v?: string) => void, sep: \"a>b\" | \"c<d{\"): void";
        assert_eq!(elide_type(s, 160), s);
        // A comma inside a parameter list ends a union, so later parameters survive.
        let u = "f(a: 1 | 2 | 3 | 4 | 5 | 6, b: number): void";
        assert_eq!(elide_type(u, 160), "f(a: 1 | 2 | 3 | … 3 more, b: number): void");
    }

    #[test]
    fn unbalanced_text_does_not_panic() {
        assert_eq!(elide_type("f(a: { x", 160), "f(a: { x");
        assert_eq!(elide_type(") > }", 160), ") > }");
    }

    #[test]
    fn long_text_is_cut_but_the_active_parameter_stays() {
        let sig = format!("f(a: {}, b: number)", "x".repeat(300));
        let (t, r) = elide_sig(&sig, Some(&(sig.find("b:").unwrap()..sig.len() - 1)), 50);
        assert!(t.ends_with("b: number)") || t.contains("b: number"), "{t}");
        assert_eq!(&t[r.unwrap()], "b: number");
        assert!(elide_type(&"y".repeat(300), 50).ends_with('…'));
    }

    #[test]
    fn first_paragraph_of_docs() {
        assert_eq!(first_paragraph("One.\nStill one.\n\nTwo."), ("One.\nStill one.", true));
        assert_eq!(first_paragraph("Only."), ("Only.", false));
        assert_eq!(first_paragraph("Does it.\n*@param* x the x"), ("Does it.", true));
        assert_eq!(first_paragraph("```ts\na\n\nb\n```\nafter"), ("```ts\na\n\nb\n```\nafter", false));
    }
}
