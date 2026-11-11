//! Format in the Query view: lays statements out with `sqlformat`, one at a time, and only keeps a result when the
//! lexer reads the same tokens in it as in the original. A formatter that knows less MySQL than the server must never
//! change what a script means, so anything it cannot prove equal stays as it was.

use super::lex::{self, Tok};
use super::Dialect;
use std::ops::Range;

/// What a format did to a script.
#[derive(Debug, Default, PartialEq)]
pub struct Formatted {
    pub text: String,
    /// Statements that were laid out again (or already were).
    pub formatted: usize,
    /// Statements left alone on purpose: a DELIMITER block, or text with an executable `/*! */` comment.
    pub special: usize,
    /// Statements whose formatted text would not read the same, so the original stays.
    pub refused: usize,
}

impl Formatted {
    /// The line the status shows after a format.
    pub fn summary(&self) -> String {
        let plural = |n: usize| if n == 1 { "statement" } else { "statements" };
        let mut s = if self.formatted == 0 && self.special == 0 && self.refused == 0 { "Nothing to format".to_string() } else { format!("Formatted {} {}", self.formatted, plural(self.formatted)) };
        if self.special > 0 {
            s.push_str(&format!(", left {} {} with a DELIMITER, /*! */ or BEGIN ATOMIC as written", self.special, plural(self.special)));
        }
        if self.refused > 0 {
            s.push_str(&format!(", left {} {} as written because formatting would change how {} reads", self.refused, plural(self.refused), if self.refused == 1 { "it" } else { "they" }));
        }
        s
    }
}

/// The text of the DELIMITER blocks: from a line that sets another delimiter to the line that sets `;` again, or to the
/// end. A lone `DELIMITER ;` line is a block of its own. A statement that touches one is not ours to reflow, because
/// the splitter of the Query view does not know about delimiters and would cut a body at its first `;`.
fn delimiter_blocks(text: &str) -> Vec<Range<usize>> {
    let toks = lex::tokens(text, Dialect::MySql);
    let mut lines: Vec<(Range<usize>, bool)> = Vec::new();
    let mut open = false;
    let mut prev: Option<&str> = None;
    for (t, r) in &toks {
        match t {
            Tok::Space | Tok::Comment => continue,
            Tok::Word if text[r.clone()].eq_ignore_ascii_case("delimiter") => {
                let start = text[..r.start].rfind('\n').map_or(0, |p| p + 1);
                let end = text[r.end..].find('\n').map_or(text.len(), |p| r.end + p);
                let leads = text[start..r.start].trim().is_empty();
                // Only where a statement may begin; a column called delimiter is not a command.
                if leads && (open || prev.is_none_or(|p| p == ";")) {
                    let restores = text[r.end..end].trim() == ";";
                    lines.push((start..end, restores));
                    open = !restores;
                }
            }
            _ => {}
        }
        prev = Some(&text[r.clone()]);
    }
    let mut out = Vec::new();
    let mut from = None;
    for (line, restores) in lines {
        match (from, restores) {
            (None, false) => from = Some(line.start),
            (None, true) => out.push(line),
            (Some(s), true) => {
                out.push(s..line.end);
                from = None;
            }
            (Some(_), false) => {}
        }
    }
    if let Some(s) = from {
        out.push(s..text.len());
    }
    out
}

/// The SQL-standard function bodies of PostgreSQL: from `BEGIN ATOMIC` to its `END`, which `CASE ... END` inside does not
/// close. The splitter does not know them and cuts at the first `;` inside, so a statement that touches one is not ours
/// to reflow: each piece would be laid out as if it stood alone.
fn atomic_blocks(text: &str) -> Vec<Range<usize>> {
    let words: Vec<(&str, Range<usize>)> = lex::tokens(text, Dialect::Postgres).into_iter().filter(|(t, _)| *t == Tok::Word).map(|(_, r)| (&text[r.clone()], r)).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < words.len() {
        if words[i].0.eq_ignore_ascii_case("begin") && words[i + 1].0.eq_ignore_ascii_case("atomic") {
            let mut depth = 0usize;
            let mut end = text.len();
            let mut j = i + 2;
            while j < words.len() {
                let w = words[j].0;
                if w.eq_ignore_ascii_case("case") {
                    depth += 1;
                } else if w.eq_ignore_ascii_case("end") {
                    if depth == 0 {
                        end = words[j].1.end;
                        break;
                    }
                    depth -= 1;
                }
                j += 1;
            }
            out.push(words[i].1.start..end);
            i = j.max(i + 1);
        }
        i += 1;
    }
    out
}

/// Tokens without whitespace, with comment text compared by its words: reindenting a block comment changes nothing the
/// server reads, but any other token must be the same bytes.
/// Each token also says whether it touched the one before it, because `b'01'` and `b '01'` have the same tokens and
/// not the same meaning.
fn significant(text: &str, toks: &[(Tok, Range<usize>)]) -> Vec<(Tok, String, bool)> {
    let mut prev_end = None;
    let mut out = Vec::new();
    for (t, r) in toks {
        if *t == Tok::Space {
            continue;
        }
        let s = &text[r.clone()];
        out.push((*t, if *t == Tok::Comment { s.split_whitespace().collect::<Vec<_>>().join(" ") } else { s.to_string() }, prev_end == Some(r.start)));
        prev_end = Some(r.end);
    }
    out
}

/// Whitespace may appear around a comma, a bracket or a semicolon, and between an operator and what it works on.
/// Between two operator characters it may not (`>=`), nor inside a literal prefix, a number or a name (`a.b`, `\N`).
fn may_split(a: &(Tok, String, bool), b: &(Tok, String, bool)) -> bool {
    let op = |t: &(Tok, String, bool)| t.0 == Tok::Punct && "=<>+-*/%&|^~!".contains(t.1.as_str());
    let free = |t: &(Tok, String, bool)| t.0 == Tok::Comment || (t.0 == Tok::Punct && matches!(t.1.as_str(), "," | "(" | ")" | ";"));
    free(a) || free(b) || (op(a) && b.0 != Tok::Punct) || (op(b) && a.0 != Tok::Punct)
}

/// The same tokens, and no two that touched in `before` pulled apart in `after`.
fn same_tokens(a: &[(Tok, String, bool)], b: &[(Tok, String, bool)]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0 && x.1 == y.1) && (1..a.len()).all(|k| !a[k].2 || b[k].2 || may_split(&a[k - 1], &a[k]))
}

/// Whether `after` is `before` with different whitespace, under every way a server may read the quotes, and still
/// one statement under each of them.
fn reads_the_same(before: &str, after: &str, d: Dialect) -> bool {
    if (0..lex::readings(d)).any(|n| !same_tokens(&significant(before, &lex::nth_reading(before, d, n)), &significant(after, &lex::nth_reading(after, d, n)))) {
        return false;
    }
    [(true, false), (true, true), (false, false), (false, true)].into_iter().all(|(bs, ansi)| lex::count_with(before, d, bs, ansi) == lex::count_with(after, d, bs, ansi))
}

fn options(d: Dialect) -> sqlformat::FormatOptions<'static> {
    // Keyword case stays as written: an identifier that happens to be a keyword must not change its spelling.
    // The PostgreSQL dialect makes sqlformat read `[`, `]` and the `->>` style operators as one piece each.
    let dialect = if d == Dialect::Postgres { sqlformat::Dialect::PostgreSql } else { sqlformat::Dialect::Generic };
    sqlformat::FormatOptions { indent: sqlformat::Indent::Spaces(2), uppercase: None, joins_as_top_level: true, dialect, ..Default::default() }
}

/// Lays out one statement, or None when the result would not read the same.
fn one(stmt: &str, d: Dialect) -> Option<String> {
    let (masked, bodies) = mask_dollar(stmt, d);
    let mut out = sqlformat::format(&masked, &sqlformat::QueryParams::None, &options(d));
    // Longest key first so `m1` never matches inside `m10`.
    for (i, b) in bodies.iter().enumerate().rev() {
        out = out.replace(&mask_name(i), b);
    }
    let out = out.trim();
    reads_the_same(stmt, out, d).then(|| out.to_string())
}

fn mask_name(i: usize) -> String {
    format!("qdollarbody{i}x")
}

/// sqlformat does not know dollar quoting and would split a body at its spaces and quotes, so each body is swapped for
/// a plain word while it lays out and put back afterwards. The check against the original still decides what is kept.
fn mask_dollar(stmt: &str, d: Dialect) -> (String, Vec<String>) {
    if d != Dialect::Postgres || !stmt.contains('$') {
        return (stmt.to_string(), Vec::new());
    }
    let (mut out, mut bodies, mut last) = (String::new(), Vec::new(), 0);
    for (t, r) in lex::tokens(stmt, d) {
        if t == Tok::Str && stmt[r.clone()].starts_with('$') {
            out.push_str(&stmt[last..r.start]);
            out.push_str(&mask_name(bodies.len()));
            bodies.push(stmt[r.clone()].to_string());
            last = r.end;
        }
    }
    out.push_str(&stmt[last..]);
    // A word of the script that looks like a mask would be swapped for a body, so nothing is masked then.
    if bodies.iter().enumerate().any(|(i, _)| stmt.contains(&mask_name(i))) {
        return (stmt.to_string(), Vec::new());
    }
    (out, bodies)
}

/// Formats every statement of `text`. Comments between statements and the `;` that end them stay where they are.
pub fn format_script(text: &str, d: Dialect) -> Formatted {
    // PostgreSQL has no DELIMITER command, but a BEGIN ATOMIC body is left alone like a DELIMITER block.
    let blocks = if d == Dialect::Postgres { atomic_blocks(text) } else { delimiter_blocks(text) };
    let mut res = Formatted::default();
    let mut last = 0;
    for (r, stmt) in lex::split_statements(text, d) {
        let special = blocks.iter().any(|b| b.start < r.end && r.start < b.end) || lex::tokens(stmt, d).iter().any(|(t, _)| *t == Tok::ExecOpen);
        if special {
            res.special += 1;
            continue;
        }
        match one(stmt, d) {
            Some(new) => {
                res.text.push_str(&text[last..r.start]);
                res.text.push_str(&new);
                last = r.end;
                res.formatted += 1;
            }
            None => res.refused += 1,
        }
    }
    res.text.push_str(&text[last..]);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_script(t: &str) -> Formatted {
        super::format_script(t, Dialect::MySql)
    }

    fn same(f: &Formatted, text: &str) {
        assert_eq!(f.text, text);
    }

    #[test]
    fn lays_out_a_select_and_keeps_the_tokens() {
        let f = format_script("select a,b from t where x=1 and y='it''s' order by a");
        assert!(f.text.contains('\n') && f.formatted == 1 && f.refused == 0, "{f:?}");
        assert!(reads_the_same("select a,b from t where x=1 and y='it''s' order by a", &f.text, Dialect::MySql));
    }

    #[test]
    fn keyword_case_and_names_stay_as_written() {
        let f = format_script("select `Order`, Status from `My Table` t join u on t.id=u.id");
        assert!(f.text.contains("`Order`") && f.text.contains("Status") && f.text.contains("`My Table`"), "{}", f.text);
        assert!(f.text.contains("select"), "{}", f.text);
    }

    #[test]
    fn comments_and_the_text_between_statements_stay() {
        let src = "-- first\nselect 1,2 from a; # note\n\n/* block\n   comment */\nselect 3 from b where c = 4;\n-- end";
        let f = format_script(src);
        assert_eq!(f.formatted, 2, "{f:?}");
        assert!(f.text.contains("-- first") && f.text.contains("# note") && f.text.ends_with(";\n-- end"), "{}", f.text);
        assert!(f.text.contains("/* block"), "{}", f.text);
        assert_eq!(lex::split_statements(&f.text, Dialect::MySql).len(), 2);
    }

    #[test]
    fn a_line_comment_never_swallows_the_next_line() {
        let f = format_script("select a -- why\n, b from t");
        assert!(reads_the_same("select a -- why\n, b from t", &f.text, Dialect::MySql), "{}", f.text);
        let f = format_script("select a # why\n, b from t where x = 1");
        assert!(reads_the_same("select a # why\n, b from t where x = 1", &f.text, Dialect::MySql), "{}", f.text);
    }

    #[test]
    fn strings_with_both_escape_readings_are_untouched() {
        for src in [r"select 'a\'b', c from t where d = 'x\\' and e = 1", r"select 'a\\', 'b' from t where x = 1", "select \"a\\\"b\" from t where x = 1", "select 'a''b', \"c\"\"d\" from t"] {
            let f = format_script(src);
            // The formatter mangles some of these (it reads `\\'` as the end of a string under one rule only); the
            // check refuses those and keeps the original, so whatever comes out reads the same as what went in.
            assert!(reads_the_same(src, &f.text, Dialect::MySql), "{src}: {}", f.text);
            // The literals themselves are byte for byte the same.
            for lit in ["'a\\'b'", "'x\\\\'", "'a\\\\'", "\"a\\\"b\"", "'a''b'", "\"c\"\"d\""] {
                assert_eq!(src.contains(lit), f.text.contains(lit), "{src}: {lit} in {}", f.text);
            }
        }
    }

    #[test]
    fn a_formatter_that_misreads_a_string_is_refused() {
        let src = r"select 'a\', 'b' from t where x = 1";
        let f = format_script(src);
        assert_eq!((f.refused, f.text.as_str()), (1, src), "{f:?}");
    }

    #[test]
    fn ansi_quotes_text_is_read_both_ways() {
        let src = "select \"a b\", \"c\" from \"t\" where \"x\" = 'y' and \"z\" > 1";
        let f = format_script(src);
        assert_eq!(f.refused, 0, "{f:?}");
        assert!(reads_the_same(src, &f.text, Dialect::MySql));
        assert!(f.text.contains("\"a b\"") && f.text.contains("'y'"));
    }

    #[test]
    fn backtick_names_with_odd_characters() {
        let src = "select `a``b`, `c d`, `e\\f` from `t;x` where `g` = 1";
        let f = format_script(src);
        assert_eq!(f.formatted + f.refused, 1);
        for lit in ["`a``b`", "`c d`", "`e\\f`", "`t;x`"] {
            assert!(f.text.contains(lit), "{lit} in {}", f.text);
        }
    }

    #[test]
    fn delimiter_blocks_and_executable_comments_are_left_alone() {
        let src = "select 1,2 from a;\nDELIMITER ;;\ncreate procedure p() begin select 1; select 2; end;;\nDELIMITER ;\nselect 3,4 from b;\nselect /*!50000 SQL_NO_CACHE */ 5,6 from c;";
        let f = format_script(src);
        assert!(f.formatted == 2 && f.special >= 3, "{f:?}");
        assert!(f.text.contains("DELIMITER ;;\ncreate procedure p() begin select 1; select 2; end;;\nDELIMITER ;\n"), "{}", f.text);
        assert!(f.text.contains("select /*!50000 SQL_NO_CACHE */ 5,6 from c;"), "{}", f.text);
        // A column called delimiter is not the command.
        let g = format_script("select\n delimiter, b from t where x = 1");
        assert_eq!((g.formatted, g.special), (1, 0), "{g:?}");
        // An unclosed block runs to the end.
        let h = format_script("select 1,2 from a;\nDELIMITER //\ncreate trigger t before insert on x for each row set new.a = 1//\n");
        assert_eq!(h.formatted, 1, "{h:?}");
        assert!(h.text.ends_with("DELIMITER //\ncreate trigger t before insert on x for each row set new.a = 1//\n"));
    }

    #[test]
    fn several_statements_keep_their_count_and_order() {
        let src = "insert into t(a,b) values (1,'x;y'),(2,'z');update t set a=a+1 where b='z';delete from t where a>5 limit 2";
        let f = format_script(src);
        assert_eq!(f.formatted, 3, "{f:?}");
        let parts: Vec<&str> = lex::split_statements(&f.text, Dialect::MySql).iter().map(|p| p.1).collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].to_lowercase().starts_with("insert") && parts[2].to_lowercase().starts_with("delete"));
        assert!(f.text.contains("'x;y'"));
    }

    #[test]
    fn nothing_to_do_and_whitespace_only() {
        same(&format_script(""), "");
        same(&format_script("  \n"), "  \n");
        same(&format_script("-- only a comment\n"), "-- only a comment\n");
        assert_eq!(format_script("").summary(), "Nothing to format");
    }

    #[test]
    fn touching_tokens_stay_touching() {
        for src in ["select b'01' from t", "select n'x' from t", "select x'4a' from t where a = 1", "select _utf8mb4'x', 1 from t", "select \\N from t", "select a.b, c from t where d >= 1"] {
            let f = format_script(src);
            assert!(reads_the_same(src, &f.text, Dialect::MySql), "{src}: {}", f.text);
            for lit in ["b'01'", "n'x'", "x'4a'", "_utf8mb4'x'", "\\N", "a.b", ">="] {
                assert_eq!(src.contains(lit), f.text.contains(lit), "{src}: {lit} in {}", f.text);
            }
        }
        assert!(!reads_the_same("select b'01' from t", "select b '01' from t", Dialect::MySql));
        assert!(!reads_the_same("select n'x'", "select n 'x'", Dialect::MySql));
        assert!(!reads_the_same("select \\N", "select \\ N", Dialect::MySql));
        assert!(!reads_the_same("select a >= 1", "select a > = 1", Dialect::MySql));
        assert!(reads_the_same("select a>=1,b", "select a >= 1,\n b", Dialect::MySql));
    }

    #[test]
    fn a_failed_check_keeps_the_original() {
        // What the lexer reads differently is refused: pretend the formatter merged two tokens.
        assert!(!reads_the_same("select 1 - 1", "select 1 -1 -", Dialect::MySql));
        assert!(!reads_the_same("select 'a' 'b'", "select 'a''b'", Dialect::MySql));
        assert!(!reads_the_same("select a -- x\nfrom t", "select a -- x from t", Dialect::MySql));
        assert!(reads_the_same("select  a ,b", "select a,\n  b", Dialect::MySql));
        let s = Formatted { formatted: 2, special: 1, refused: 1, text: String::new() }.summary();
        assert!(s.contains("Formatted 2 statements") && s.contains("DELIMITER") && s.contains("would change"), "{s}");
    }

    fn pg(t: &str) -> Formatted {
        super::format_script(t, Dialect::Postgres)
    }

    /// Whatever Format does to PostgreSQL text, the lexer must read the same tokens afterwards.
    fn pg_safe(src: &str) -> Formatted {
        let f = pg(src);
        assert!(f.text == src || reads_the_same(src, &f.text, Dialect::Postgres), "{src}: {}", f.text);
        f
    }

    #[test]
    fn pg_lays_out_a_select() {
        let f = pg_safe("select a,b from t where x=1 and y='it''s' order by a");
        assert!(f.text.contains('\n') && f.formatted == 1 && f.refused == 0, "{f:?}");
    }

    #[test]
    fn pg_dollar_quoted_bodies_stay_byte_for_byte() {
        for (src, body) in [
            ("create function f() returns int language sql as $$select  1  ,2 ; select 3$$", "$$select  1  ,2 ; select 3$$"),
            ("create function f() returns int language plpgsql as $fn$ begin return 1; end $fn$", "$fn$ begin return 1; end $fn$"),
            ("do $body$ begin raise notice 'a;b'; end $body$", "$body$ begin raise notice 'a;b'; end $body$"),
        ] {
            let f = pg_safe(src);
            assert!(f.text.contains(body), "{src}: {}", f.text);
        }
    }

    #[test]
    fn pg_casts_never_split() {
        let f = pg_safe("select a::int, b::text::varchar(3), '1'::date from t where c::int>1");
        assert!(f.text.contains("a::int") && !f.text.contains(": :"), "{}", f.text);
        assert!(f.text.contains("::text::varchar"), "{}", f.text);
    }

    #[test]
    fn pg_operators_stay_whole() {
        for op in ["->>", "->", "#>>", "#>", "@>", "<@", "?|", "?&", "||", "!=", "<>", "<=", ">=", "@@", "~*", "!~", "&&"] {
            let src = format!("select a {op} b from t where c = 1");
            let f = pg_safe(&src);
            assert!(f.text == src || f.text.contains(op), "{op}: {}", f.text);
        }
        let f = pg_safe("select data->>'k', data#>>'{a,b}', tags@>array['x'], j?|array['a'] from t");
        assert!(f.text.contains("->>") && f.text.contains("#>>") && f.text.contains("@>") || f.refused == 1, "{}", f.text);
    }

    #[test]
    fn pg_arrays_subscripts_and_parameters() {
        for src in ["select a[1:2], b[3], array[1,2,3], array[[1,2],[3,4]] from t where id = $1 and n > $2", "select * from t where a = any($1) and b = $10", "select (a)[1], c[1][2] from t"] {
            let f = pg_safe(src);
            assert!(f.refused == 0 || f.text == src, "{src}: {f:?}");
            for lit in ["$1", "$2", "$10"] {
                assert_eq!(src.contains(lit), f.text.contains(lit), "{src}: {}", f.text);
            }
        }
        let f = pg_safe("select a[1:2], array[1,2] from t");
        assert!(f.text.contains("a[1:2]") || f.refused == 1, "{}", f.text);
    }

    #[test]
    fn pg_strings_and_quoted_names_stay() {
        for src in [
            "select E'a\\'b\\n', 1 from t where x=2",
            "select U&'d\\0061t\\+000061', 1 from t where x=2",
            "select U&\"d\\0061t\" , \"a\"\"b\" from \"My Table\" where x=2",
            "select \"Order\", \"it's\"\"\" from t where x=2",
            "select 'it''s', $q$ a'b $q$ from t where x=2",
            "select B'101', X'1f', 1e3, 1.5e-2, .5 from t where x=2",
        ] {
            let f = pg_safe(src);
            // Either laid out with every literal intact or left as written.
            for lit in ["E'a\\'b\\n'", "U&'d\\0061t\\+000061'", "U&\"d\\0061t\"", "\"a\"\"b\"", "\"it's\"\"\"", "'it''s'", "$q$ a'b $q$", "B'101'", "X'1f'", "1e3", "1.5e-2"] {
                assert_eq!(src.contains(lit), f.text.contains(lit), "{src}: {lit} in {}", f.text);
            }
        }
    }

    #[test]
    fn pg_escape_strings_are_laid_out_and_unicode_escapes_refused() {
        let e = pg_safe("select E'a\\'b', 1 from t where x=2");
        assert!(e.formatted == 1 && e.text.contains("E'a\\'b'"), "{e:?}");
        // sqlformat cuts `U&` from its quote, which would be another token, so the statement stays as written.
        for src in ["select U&'d\\0061t', 1 from t where x=2", "select U&\"d\\0061t\", 1 from t where x=2"] {
            let f = pg_safe(src);
            assert_eq!((f.text.as_str(), f.refused), (src, 1), "{f:?}");
        }
        let q = pg_safe("select \"a\"\"b\", 1 from t where x=2");
        assert!(q.formatted == 1 && q.text.contains("\"a\"\"b\""), "{q:?}");
    }

    #[test]
    fn pg_nested_block_comments_stay() {
        let src = "select 1 /* a /* nested ; */ still comment ; */, 2 from t where x = 1";
        let f = pg_safe(src);
        assert!(f.text.contains("/* a /* nested ; */ still comment ; */") || f.refused == 1, "{}", f.text);
        assert_eq!(lex::count_with(&f.text, Dialect::Postgres, false, false), 1);
    }

    #[test]
    fn pg_keywords_stay_as_written() {
        let f = pg_safe("select a from t where a is distinct from b and c is not distinct from d and e is not null");
        assert!(f.text.contains("distinct from") && f.text.contains("not distinct from"), "{}", f.text);
        let g = pg_safe("select * from t order by a nulls last, b desc limit 5 offset 2");
        assert!(g.text.contains("nulls last"), "{}", g.text);
    }

    #[test]
    fn pg_atomic_function_body_stays_as_written() {
        let src = "create function f(a int) returns int language sql begin atomic select a + 1; select case when a > 1 then 1 end; end;\nselect a,b from t where c=1";
        let f = pg_safe(src);
        assert!(f.special >= 1 && f.text.starts_with("create function f(a int) returns int language sql begin atomic select a + 1; select case when a > 1 then 1 end; end;"), "{f:?}");
        assert_eq!(f.formatted, 1, "{f:?}");
        assert!(f.text.contains("from\n  t"), "{}", f.text);
        // An unclosed body runs to the end of the text.
        let g = pg_safe("create function f() returns int begin atomic select 1;\nselect  2");
        assert_eq!((g.formatted, g.special >= 1), (0, true), "{g:?}");
    }

    #[test]
    fn pg_several_statements_and_comments() {
        let src = "-- first\nselect 1,2 from a; /* mid */ select $$x;y$$ , 2 from b where c=1;\n-- end";
        let f = pg_safe(src);
        assert_eq!(f.formatted, 2, "{f:?}");
        assert!(f.text.contains("select\n  $$x;y$$,") || f.text.contains("$$x;y$$"), "{}", f.text);
        assert!(f.text.starts_with("-- first\n") && f.text.ends_with("\n-- end") && f.text.contains("$$x;y$$"), "{}", f.text);
        assert_eq!(lex::count_with(&f.text, Dialect::Postgres, false, false), 2);
    }

    #[test]
    fn pg_refuses_what_would_read_differently() {
        assert!(!reads_the_same("select a::int", "select a: :int", Dialect::Postgres));
        assert!(!reads_the_same("select a ->> b", "select a - >> b", Dialect::Postgres));
        assert!(!reads_the_same("select $$a$$", "select $$ a$$", Dialect::Postgres));
        assert!(!reads_the_same("select a[1:2]", "select a[1 :2]", Dialect::Postgres));
        assert!(!reads_the_same("select $1", "select $ 1", Dialect::Postgres));
        assert!(!reads_the_same("select E'a'", "select E 'a'", Dialect::Postgres));
        assert!(reads_the_same("select a,b", "select a,\n  b", Dialect::Postgres));
    }
}
