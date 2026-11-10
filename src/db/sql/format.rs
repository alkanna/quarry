//! Format in the Query view: lays statements out with `sqlformat`, one at a time, and only keeps a result when the
//! lexer reads the same tokens in it as in the original. A formatter that knows less MySQL than the server must never
//! change what a script means, so anything it cannot prove equal stays as it was.

use super::lex::{self, Tok};
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
            s.push_str(&format!(", left {} {} with a DELIMITER or /*! */ as written", self.special, plural(self.special)));
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
    let toks = lex::tokens(text);
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

/// Tokens without whitespace, with comment text compared by its words: reindenting a block comment changes nothing the
/// server reads, but any other token must be the same bytes.
fn significant(text: &str, toks: &[(Tok, Range<usize>)]) -> Vec<(Tok, String)> {
    toks.iter()
        .filter(|(t, _)| *t != Tok::Space)
        .map(|(t, r)| {
            let s = &text[r.clone()];
            (*t, if *t == Tok::Comment { s.split_whitespace().collect::<Vec<_>>().join(" ") } else { s.to_string() })
        })
        .collect()
}

/// Whether `after` is `before` with different whitespace, under every way a server may read the quotes, and still
/// one statement under each of them.
fn reads_the_same(before: &str, after: &str) -> bool {
    let (a, b) = (lex::every_reading(before), lex::every_reading(after));
    if a.len() != b.len() || a.iter().zip(&b).any(|(x, y)| significant(before, x) != significant(after, y)) {
        return false;
    }
    [(true, false), (true, true), (false, false), (false, true)].into_iter().all(|(bs, ansi)| lex::count_with(before, bs, ansi) == lex::count_with(after, bs, ansi))
}

fn options() -> sqlformat::FormatOptions<'static> {
    // Keyword case stays as written: an identifier that happens to be a keyword must not change its spelling.
    sqlformat::FormatOptions { indent: sqlformat::Indent::Spaces(2), uppercase: None, joins_as_top_level: true, ..Default::default() }
}

/// Lays out one statement, or None when the result would not read the same.
fn one(stmt: &str) -> Option<String> {
    let out = sqlformat::format(stmt, &sqlformat::QueryParams::None, &options());
    let out = out.trim();
    reads_the_same(stmt, out).then(|| out.to_string())
}

/// Formats every statement of `text`. Comments between statements and the `;` that end them stay where they are.
pub fn format_script(text: &str) -> Formatted {
    let blocks = delimiter_blocks(text);
    let mut res = Formatted::default();
    let mut last = 0;
    for (r, stmt) in lex::split_statements(text) {
        let special = blocks.iter().any(|b| b.start < r.end && r.start < b.end) || lex::tokens(stmt).iter().any(|(t, _)| *t == Tok::ExecOpen);
        if special {
            res.special += 1;
            continue;
        }
        match one(stmt) {
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

    fn same(f: &Formatted, text: &str) {
        assert_eq!(f.text, text);
    }

    #[test]
    fn lays_out_a_select_and_keeps_the_tokens() {
        let f = format_script("select a,b from t where x=1 and y='it''s' order by a");
        assert!(f.text.contains('\n') && f.formatted == 1 && f.refused == 0, "{f:?}");
        assert!(reads_the_same("select a,b from t where x=1 and y='it''s' order by a", &f.text));
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
        assert_eq!(lex::split_statements(&f.text).len(), 2);
    }

    #[test]
    fn a_line_comment_never_swallows_the_next_line() {
        let f = format_script("select a -- why\n, b from t");
        assert!(reads_the_same("select a -- why\n, b from t", &f.text), "{}", f.text);
        let f = format_script("select a # why\n, b from t where x = 1");
        assert!(reads_the_same("select a # why\n, b from t where x = 1", &f.text), "{}", f.text);
    }

    #[test]
    fn strings_with_both_escape_readings_are_untouched() {
        for src in [r"select 'a\'b', c from t where d = 'x\\' and e = 1", r"select 'a\\', 'b' from t where x = 1", "select \"a\\\"b\" from t where x = 1", "select 'a''b', \"c\"\"d\" from t"] {
            let f = format_script(src);
            // The formatter mangles some of these (it reads `\\'` as the end of a string under one rule only); the
            // check refuses those and keeps the original, so whatever comes out reads the same as what went in.
            assert!(reads_the_same(src, &f.text), "{src}: {}", f.text);
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
        assert!(reads_the_same(src, &f.text));
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
        let parts: Vec<&str> = lex::split_statements(&f.text).iter().map(|p| p.1).collect();
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
    fn a_failed_check_keeps_the_original() {
        // What the lexer reads differently is refused: pretend the formatter merged two tokens.
        assert!(!reads_the_same("select 1 - 1", "select 1 -1 -"));
        assert!(!reads_the_same("select 'a' 'b'", "select 'a''b'"));
        assert!(!reads_the_same("select a -- x\nfrom t", "select a -- x from t"));
        assert!(reads_the_same("select  a ,b", "select a,\n  b"));
        let s = Formatted { formatted: 2, special: 1, refused: 1, text: String::new() }.summary();
        assert!(s.contains("Formatted 2 statements") && s.contains("DELIMITER") && s.contains("would change"), "{s}");
    }
}
