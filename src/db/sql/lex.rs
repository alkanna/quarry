//! A small MySQL tokenizer, the statement splitter and the read-only guard. Shared by the worker,
//! the highlighter and autocomplete, so all of them agree on where strings and comments end.

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Space,
    Comment,
    /// `/*!`, `/*!80000` or MariaDB's `/*M!`: the server runs what follows, so it is code.
    ExecOpen,
    /// The `*/` that ends an executable comment.
    ExecClose,
    Word,
    /// A backtick identifier, quotes included.
    Quoted,
    /// A '...' or "..." string, quotes included.
    Str,
    Punct,
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// Splits text into tokens that cover it completely. Delimiters are ASCII, so every range sits on a
/// char boundary. Unterminated strings and comments run to the end of the text.
pub fn tokens(text: &str) -> Vec<(Tok, Range<usize>)> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut exec = false;
    let mut i = 0;
    while i < n {
        let s = i;
        let c = b[i];
        let next = b.get(i + 1).copied();
        let tok = if c.is_ascii_whitespace() {
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            Tok::Space
        } else if c == b'\'' || c == b'"' || c == b'`' {
            i += 1;
            while i < n {
                // Backticks have no backslash escape, quotes do (default sql_mode).
                if b[i] == b'\\' && c != b'`' {
                    i += 2;
                } else if b[i] == c {
                    if b.get(i + 1) == Some(&c) {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            i = i.min(n);
            if c == b'`' { Tok::Quoted } else { Tok::Str }
        } else if c == b'#' || (c == b'-' && next == Some(b'-') && b.get(i + 2).is_none_or(|x| x.is_ascii_whitespace() || x.is_ascii_control())) {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            Tok::Comment
        } else if c == b'/' && next == Some(b'*') {
            let mut j = i + 2;
            if b.get(j) == Some(&b'M') && b.get(j + 1) == Some(&b'!') {
                j += 1;
            }
            if b.get(j) == Some(&b'!') {
                j += 1;
                while j < n && b[j].is_ascii_digit() {
                    j += 1;
                }
                exec = true;
                i = j;
                Tok::ExecOpen
            } else {
                i = text[i + 2..].find("*/").map_or(n, |p| i + 2 + p + 2);
                Tok::Comment
            }
        } else if exec && c == b'*' && next == Some(b'/') {
            exec = false;
            i += 2;
            Tok::ExecClose
        } else if c == b'@' || is_word(c) {
            while i < n && b[i] == b'@' {
                i += 1;
            }
            while i < n && is_word(b[i]) {
                i += 1;
            }
            Tok::Word
        } else {
            i += 1;
            Tok::Punct
        };
        out.push((tok, s..i));
    }
    out
}

/// Each statement as (range without the `;`, end including the `;`). Empty ones are dropped.
fn split(text: &str) -> Vec<(Range<usize>, usize)> {
    let mut out = Vec::new();
    let (mut start, mut last, mut code) = (None, 0, false);
    for (t, r) in tokens(text) {
        match t {
            Tok::Space => {}
            Tok::Punct if &text[r.clone()] == ";" => {
                if let (Some(s), true) = (start, code) {
                    out.push((s..last, r.end));
                }
                (start, code) = (None, false);
            }
            _ => {
                start.get_or_insert(r.start);
                last = r.end;
                code |= matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct);
            }
        }
    }
    if let (Some(s), true) = (start, code) {
        out.push((s..last, text.len()));
    }
    out
}

/// Splits on `;` outside strings and comments. Ranges are byte offsets into `text`, trimmed of
/// surrounding whitespace, so the editor can map a caret to a statement.
pub fn split_statements(text: &str) -> Vec<(Range<usize>, &str)> {
    split(text).into_iter().map(|(r, _)| (r.clone(), &text[r])).collect()
}

/// The statement the caret belongs to. A caret right after a `;` stays with that statement, and one
/// past the last statement picks the last one.
pub fn statement_at(text: &str, caret: usize) -> Option<Range<usize>> {
    let all = split(text);
    all.iter().find(|(_, semi)| *semi >= caret).or(all.last()).map(|(r, _)| r.clone())
}

type Code<'a> = (Tok, &'a str);

fn is(t: &Code, kw: &str) -> bool {
    t.0 == Tok::Word && t.1.eq_ignore_ascii_case(kw)
}

fn punct(t: &Code, p: &str) -> bool {
    t.0 == Tok::Punct && t.1 == p
}

const REFUSED: &str = "Unlock writes first.";

/// Allow-list on the first keyword, because the session guard (`TRANSACTION READ ONLY`) does not
/// stop everything, for example SET SESSION or INTO OUTFILE. The text between `/*!` and `*/` is
/// checked as code since the server runs it. Several statements are all checked.
pub fn read_only_check(stmt: &str) -> Result<(), String> {
    let code: Vec<Code> = tokens(stmt).into_iter().filter(|(t, _)| matches!(t, Tok::Word | Tok::Quoted | Tok::Str | Tok::Punct)).map(|(t, r)| (t, &stmt[r])).collect();
    code.split(|t| punct(t, ";")).try_for_each(check_one)
}

fn check_one(part: &[Code]) -> Result<(), String> {
    if part.windows(2).any(|w| is(&w[0], "INTO") && (is(&w[1], "OUTFILE") || is(&w[1], "DUMPFILE"))) {
        return Err("INTO OUTFILE writes a file on the server and is not allowed.".into());
    }
    // A statement may open with parentheses, as in (SELECT 1) UNION (SELECT 2).
    let at = part.iter().take_while(|t| punct(t, "(")).count();
    let Some(first) = part.get(at) else { return Ok(()) };
    if first.0 != Tok::Word {
        return Err(format!("This statement is not allowed on a read-only connection. {REFUSED}"));
    }
    let rest = &part[at + 1..];
    let verb = first.1.to_ascii_uppercase();
    match verb.as_str() {
        "SELECT" | "TABLE" | "VALUES" | "SHOW" | "HELP" | "USE" => Ok(()),
        "WITH" => check_with(rest),
        "DESC" | "DESCRIBE" | "EXPLAIN" => match rest.first() {
            // Only ANALYZE runs the statement it explains.
            Some(t) if is(t, "ANALYZE") => {
                let inner = &rest[1..];
                let inner = if inner.first().is_some_and(|t| is(t, "FORMAT")) { inner.get(3..).unwrap_or(&[]) } else { inner };
                if inner.is_empty() { Err(format!("EXPLAIN ANALYZE needs a statement to run. {REFUSED}")) } else { check_one(inner) }
            }
            _ => Ok(()),
        },
        "SET" => {
            // Every comma separated assignment outside parentheses must start with @name.
            let mut depth = 0i32;
            let mut starts = vec![0];
            for (i, t) in rest.iter().enumerate() {
                if punct(t, "(") {
                    depth += 1;
                } else if punct(t, ")") {
                    depth -= 1;
                } else if depth == 0 && punct(t, ",") {
                    starts.push(i + 1);
                }
            }
            let user_var = |i: &usize| rest.get(*i).is_some_and(|t| t.0 == Tok::Word && t.1.starts_with('@') && !t.1.starts_with("@@"));
            if rest.is_empty() || !starts.iter().all(user_var) {
                return Err("Only SET @variable is allowed on a read-only connection.".into());
            }
            Ok(())
        }
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "TRUNCATE" | "LOAD" | "CREATE" | "ALTER" | "DROP" | "RENAME" | "GRANT" | "REVOKE" => Err(format!("{verb} writes data and this connection is read-only. {REFUSED}")),
        _ => Err(format!("{verb} is not allowed on a read-only connection. {REFUSED}")),
    }
}

/// MySQL 8 lets a CTE feed INSERT, UPDATE, DELETE or REPLACE, so look for them outside parentheses.
fn check_with(rest: &[Code]) -> Result<(), String> {
    let mut depth = 0i32;
    for (i, t) in rest.iter().enumerate() {
        if punct(t, "(") {
            depth += 1;
        } else if punct(t, ")") {
            depth -= 1;
        } else if depth == 0 && t.0 == Tok::Word {
            let w = t.1.to_ascii_uppercase();
            let called = rest.get(i + 1).is_some_and(|n| punct(n, "("));
            let locking = i > 0 && is(&rest[i - 1], "FOR");
            let write = match w.as_str() {
                "INSERT" | "REPLACE" => !called,
                "UPDATE" => !locking,
                "DELETE" => true,
                _ => false,
            };
            if write {
                return Err(format!("{w} inside WITH writes data and this connection is read-only. {REFUSED}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<&str> {
        split_statements(s).into_iter().map(|(_, t)| t).collect()
    }

    #[test]
    fn splits_outside_strings_and_comments() {
        assert_eq!(texts("SELECT 1; SELECT 2;"), ["SELECT 1", "SELECT 2"]);
        assert_eq!(texts("SELECT 'a;b', \"c;d\", `e;f`; SELECT 2"), ["SELECT 'a;b', \"c;d\", `e;f`", "SELECT 2"]);
        assert_eq!(texts("SELECT 1 -- x; y\n; SELECT 2 # a;b\n; SELECT /* ; */ 3"), ["SELECT 1 -- x; y", "SELECT 2 # a;b", "SELECT /* ; */ 3"]);
        assert_eq!(texts(";; ;\n-- only a comment\n;/* c */;"), Vec::<&str>::new());
    }

    #[test]
    fn quotes_escape_in_both_ways() {
        assert_eq!(texts(r"SELECT 'it\'s;' ; SELECT 2"), [r"SELECT 'it\'s;'", "SELECT 2"]);
        assert_eq!(texts("SELECT 'it''s;'; SELECT 2"), ["SELECT 'it''s;'", "SELECT 2"]);
        assert_eq!(texts("SELECT `a``;b`; SELECT 2"), ["SELECT `a``;b`", "SELECT 2"]);
        // A backslash does not escape inside backticks.
        assert_eq!(texts(r"SELECT `a\`; SELECT 2"), [r"SELECT `a\`", "SELECT 2"]);
        assert_eq!(texts("SELECT 'open;"), ["SELECT 'open;"]);
    }

    #[test]
    fn double_dash_needs_a_space() {
        assert_eq!(texts("SELECT 1--1; SELECT 2"), ["SELECT 1--1", "SELECT 2"]);
        assert_eq!(texts("SELECT 1 --; SELECT 2"), ["SELECT 1 --", "SELECT 2"]);
        assert_eq!(texts("SELECT 1 -- ; SELECT 2"), ["SELECT 1 -- ; SELECT 2"]);
        assert_eq!(texts("SELECT 1 --\t; SELECT 2"), ["SELECT 1 --\t; SELECT 2"]);
        assert_eq!(texts("SELECT 1 --"), ["SELECT 1 --"]);
    }

    #[test]
    fn executable_comments_are_code() {
        assert_eq!(texts("SELECT 1 /*! ; DELETE FROM t */"), ["SELECT 1 /*!", "DELETE FROM t */"]);
        assert_eq!(texts("/*!80000 SELECT 1 */; SELECT 2"), ["/*!80000 SELECT 1 */", "SELECT 2"]);
        assert_eq!(texts("/*! */"), Vec::<&str>::new());
        let t = tokens("/*!80000 x */");
        assert_eq!(t[0].0, Tok::ExecOpen);
        assert_eq!(t.last().unwrap().0, Tok::ExecClose);
        // Without the bang it is an ordinary comment.
        assert_eq!(tokens("/* x */ y").iter().filter(|(t, _)| *t == Tok::Comment).count(), 1);
    }

    #[test]
    fn ranges_find_the_statement_under_the_caret() {
        let s = "SELECT 1;  SELECT 2;\nSELECT 3";
        let r = split_statements(s);
        assert_eq!(r[1].0, 11..19);
        assert_eq!(statement_at(s, 0), Some(0..8));
        assert_eq!(statement_at(s, 9), Some(0..8));
        assert_eq!(statement_at(s, 10), Some(11..19));
        assert_eq!(statement_at(s, 15), Some(11..19));
        assert_eq!(statement_at(s, 21), Some(21..29));
        assert_eq!(statement_at(s, 999), Some(21..29));
        assert_eq!(statement_at("  ;  ", 1), None);
    }

    #[test]
    fn non_ascii_text_keeps_char_boundaries() {
        let s = "SELECT 'é;ü', é; SELECT 2";
        assert_eq!(texts(s), ["SELECT 'é;ü', é", "SELECT 2"]);
    }

    fn refused(s: &str) -> String {
        read_only_check(s).expect_err(s)
    }

    #[test]
    fn reading_statements_pass() {
        for s in [
            "SELECT 1",
            "select * from t where a = 'DELETE'",
            "(SELECT 1) UNION (SELECT 2)",
            "SHOW TABLES",
            "DESC t",
            "DESCRIBE t",
            "EXPLAIN SELECT 1",
            "EXPLAIN UPDATE t SET a = 1",
            "TABLE t",
            "VALUES ROW(1)",
            "HELP 'x'",
            "USE shop",
            "SET @x = 1",
            "set @a := 1, @b = 2",
            "SET @x = IF(a, b, c)",
            "SELECT 1 -- DELETE FROM t",
            "SELECT 1 /* DELETE FROM t */",
            "SELECT * FROM t FOR UPDATE",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "WITH c AS (SELECT 1) SELECT REPLACE(a, 'b', 'c'), INSERT(a, 1, 1, 'x') FROM c",
            "WITH c AS (SELECT 1) SELECT * FROM c FOR UPDATE",
            "SELECT 1; SELECT 2",
            "",
            "  -- nothing",
            "/*!80000 SELECT 1 */",
            "EXPLAIN ANALYZE SELECT 1",
            "EXPLAIN ANALYZE FORMAT=TREE SELECT 1",
            "SELECT 'INTO OUTFILE'",
            "SELECT `into` outfile FROM t",
        ] {
            assert_eq!(read_only_check(s), Ok(()), "{s}");
        }
    }

    #[test]
    fn writing_statements_are_refused_with_a_plain_message() {
        assert_eq!(refused("UPDATE t SET a = 1"), "UPDATE writes data and this connection is read-only. Unlock writes first.");
        assert!(refused("delete from t").starts_with("DELETE writes data"));
        assert!(refused("INSERT INTO t VALUES (1)").starts_with("INSERT writes"));
        assert!(refused("DROP TABLE t").starts_with("DROP writes"));
        assert!(refused("CALL p()").starts_with("CALL is not allowed"));
        assert!(refused("ANALYZE TABLE t").starts_with("ANALYZE is not allowed"));
        assert!(refused("-- c\nDELETE FROM t").starts_with("DELETE"));
        assert!(refused("SELECT 1; DELETE FROM t").starts_with("DELETE"));
    }

    #[test]
    fn set_only_allows_user_variables() {
        for s in [
            "SET SESSION transaction_read_only = 0",
            "SET GLOBAL x = 1",
            "SET @@session.sql_mode = ''",
            "SET @@global.x = 1",
            "SET TRANSACTION READ WRITE",
            "SET NAMES utf8mb4",
            "SET autocommit = 1",
            "SET @x = 1, @@sql_mode = ''",
            "SET @x = 1, GLOBAL y = 2",
            "SET",
            "SET STATEMENT max_statement_time=1 FOR DELETE FROM t",
        ] {
            assert_eq!(refused(s), "Only SET @variable is allowed on a read-only connection.", "{s}");
        }
    }

    #[test]
    fn cte_with_a_top_level_write_is_refused() {
        assert!(refused("WITH c AS (SELECT 1) DELETE FROM t WHERE a IN (SELECT * FROM c)").contains("DELETE inside WITH"));
        assert!(refused("WITH c AS (SELECT 1) UPDATE t SET a = 1").contains("UPDATE inside WITH"));
        assert!(refused("WITH c AS (SELECT 1) INSERT INTO t SELECT * FROM c").contains("INSERT inside WITH"));
        assert!(refused("WITH c AS (SELECT 1) REPLACE INTO t SELECT * FROM c").contains("REPLACE inside WITH"));
    }

    #[test]
    fn explain_analyze_runs_its_statement() {
        assert!(refused("EXPLAIN ANALYZE DELETE FROM t").starts_with("DELETE writes"));
        assert!(refused("EXPLAIN ANALYZE FORMAT=TREE UPDATE t SET a = 1").starts_with("UPDATE writes"));
        assert!(refused("EXPLAIN ANALYZE").contains("needs a statement"));
        assert!(refused("DESCRIBE ANALYZE WITH c AS (SELECT 1) DELETE FROM t").contains("inside WITH"));
    }

    #[test]
    fn outfile_is_refused_anywhere() {
        assert!(refused("SELECT * FROM t INTO OUTFILE '/tmp/x'").contains("OUTFILE"));
        assert!(refused("select 1 into\n dumpfile '/tmp/x'").contains("OUTFILE"));
        assert!(refused("TABLE t INTO OUTFILE '/tmp/x'").contains("OUTFILE"));
        assert!(refused("SELECT 1 /*! INTO OUTFILE '/tmp/x' */").contains("OUTFILE"));
    }

    #[test]
    fn executable_comments_are_checked_as_code() {
        assert!(refused("SELECT 1 /*! ; DELETE FROM t */").starts_with("DELETE"));
        assert!(refused("/*!DELETE FROM t*/").starts_with("DELETE"));
        assert!(refused("/*!50000 DROP TABLE t */").starts_with("DROP"));
        assert!(refused("/*M!100100 DELETE FROM t */").starts_with("DELETE"));
        assert!(refused("SELECT 1 /*!80000 INTO OUTFILE 'x' */").contains("OUTFILE"));
        assert_eq!(read_only_check("SELECT 1 /*!80000 , 2 */"), Ok(()));
    }
}
