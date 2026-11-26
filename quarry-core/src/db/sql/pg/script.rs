//! Splitting a PostgreSQL script that arrives in chunks into statements, for running a dump file. It reads what the
//! server reads: dollar quotes with tags, E'' strings, nested block comments, quoted names, and `;` inside
//! parentheses or a SQL-standard function body (BEGIN ATOMIC ... END), none of which end a statement. It refuses what a
//! file for psql can hold that is not SQL: backslash commands and the data lines that follow a COPY FROM stdin.
//! A statement is only returned once its end was seen inside the text fed so far, so a statement is never cut by a
//! chunk edge and a dump of any size needs only its longest statement in memory.

fn ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn ident_cont(c: u8) -> bool {
    ident_start(c) || c.is_ascii_digit() || c == b'$'
}

/// What is open at the scan position.
#[derive(Clone, Debug, PartialEq, Eq)]
enum In {
    Code,
    /// A '...' string; true when a backslash escapes inside it (E'' strings, and plain ones when
    /// standard_conforming_strings is off).
    Str(bool),
    Quoted,
    Dollar(Vec<u8>),
    Line,
    Block(u32),
}

/// One statement of the script and where it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgPiece {
    pub text: String,
    /// The line of the file its first character is on, from 1.
    pub line: u64,
}

pub struct PgSplitter {
    buf: Vec<u8>,
    /// Start of the pending statement.
    pos: usize,
    /// The next byte to look at.
    scan: usize,
    st: In,
    /// The pending statement holds code. One of only comments is not sent, since the server answers "empty query".
    code: bool,
    /// Where the first code of the pending statement is, which is the line errors point at (a comment may come before it).
    code_at: usize,
    /// Open parentheses, inside which a `;` does not end a statement (a rule with several actions).
    depth: u32,
    /// The words that opened the statement, lower case, up to the point where they decide what it is.
    head: Vec<String>,
    /// Inside the body of a SQL-standard function: the BEGIN/CASE ... END nesting left open.
    atomic: u32,
    /// The pending statement is CREATE [OR REPLACE] FUNCTION or PROCEDURE, the only place BEGIN ATOMIC opens.
    routine: bool,
    /// The last word read, to tell BEGIN ATOMIC from a plain BEGIN.
    last: String,
    plain_backslash: bool,
    eof: bool,
    /// Bytes dropped from the front of `buf`, so offsets count from the start of the file.
    dropped: u64,
    /// The line number at byte `lc_at` of the buffer, kept so finding the line of each statement never rescans text
    /// already counted.
    lc_at: usize,
    lc_line: u64,
    max: usize,
}

const MAX_STATEMENT: usize = 256 << 20;

impl PgSplitter {
    /// `plain_backslash` is true when the session has standard_conforming_strings off, which makes a plain string honour
    /// backslashes. A session of Quarry never has it off, but a script can switch it.
    pub fn new(plain_backslash: bool) -> PgSplitter {
        PgSplitter { buf: Vec::new(), pos: 0, scan: 0, st: In::Code, code: false, code_at: 0, depth: 0, head: Vec::new(), atomic: 0, routine: false, last: String::new(), plain_backslash, eof: false, dropped: 0, lc_at: 0, lc_line: 1, max: MAX_STATEMENT }
    }

    pub fn set_plain_backslash(&mut self, on: bool) {
        self.plain_backslash = on;
    }

    #[cfg(test)]
    fn set_max(&mut self, max: usize) {
        self.max = max;
    }

    /// Bytes of the script read so far, for a progress bar.
    pub fn offset(&self) -> u64 {
        self.dropped + self.pos as u64
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.pos > 0 {
            self.line_at(self.pos);
            self.lc_at -= self.pos;
            self.code_at = self.code_at.saturating_sub(self.pos);
            self.buf.drain(..self.pos);
            self.dropped += self.pos as u64;
            self.scan -= self.pos;
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// No more text follows, so a last statement without its `;` is complete.
    pub fn finish(&mut self) {
        self.eof = true;
    }

    /// The line of byte `i` of the buffer, from 1.
    fn line_at(&mut self, i: usize) -> u64 {
        let i = i.min(self.buf.len());
        let nl = |s: &[u8]| s.iter().filter(|&&b| b == b'\n').count() as u64;
        if i >= self.lc_at {
            self.lc_line += nl(&self.buf[self.lc_at..i]);
        } else {
            self.lc_line -= nl(&self.buf[i..self.lc_at]);
        }
        self.lc_at = i;
        self.lc_line
    }

    fn wait(&mut self, i: usize) -> Result<Option<PgPiece>, String> {
        self.scan = i;
        if self.buf.len() - self.pos > self.max {
            return Err(format!("Line {}: a statement is longer than {} MB. A quote, dollar quote or comment left open earlier in the file reads the same way.", self.line_at(self.pos), self.max >> 20));
        }
        Ok(None)
    }

    /// The pending statement up to `end`, with the next one starting at `next`. None when it held no code.
    fn take(&mut self, end: usize, next: usize) -> Result<Option<PgPiece>, String> {
        let had = self.code;
        let line = self.line_at(if had { self.code_at } else { self.pos });
        let raw = &self.buf[self.pos..end];
        let text = std::str::from_utf8(raw).map(|s| s.trim().to_string()).map_err(|x| format!("Line {line}: the file is not valid UTF-8 (byte {} of the statement), so it cannot be sent to the server.", x.valid_up_to() + 1));
        (self.pos, self.scan, self.st, self.code, self.depth, self.atomic, self.routine, self.last) = (next, next, In::Code, false, 0, 0, false, String::new());
        self.head.clear();
        if !had {
            return Ok(None);
        }
        let text = text?;
        refuse(&text, line)?;
        Ok(Some(PgPiece { text, line }))
    }

    /// The next statement, None when more text is needed (or the script is over).
    pub fn next_piece(&mut self) -> Result<Option<PgPiece>, String> {
        let n = self.buf.len();
        let mut i = self.scan;
        loop {
            if i >= n {
                if !self.eof {
                    return self.wait(n);
                }
                // An open quote or comment at the end of the file is an error the server would report; the text goes
                // out as it is so the server says so.
                return self.take(n, n);
            }
            let (at, was_code) = (i, self.code);
            let eof = self.eof;
            // Up to this many bytes decide a mark, so a chunk edge inside one waits for more.
            let need = |i: usize, k: usize| i + k >= n && !eof;
            let b = &self.buf;
            match self.st.clone() {
                In::Str(bs) => match b[i..].iter().position(|&c| c == b'\'' || (bs && c == b'\\')) {
                    None => i = n,
                    Some(p) => {
                        i += p;
                        if need(i, 1) {
                            return self.wait(i);
                        }
                        if b[i] == b'\\' || b.get(i + 1) == Some(&b'\'') {
                            i += 2;
                        } else {
                            i += 1;
                            self.st = In::Code;
                        }
                    }
                },
                In::Quoted => match b[i..].iter().position(|&c| c == b'"') {
                    None => i = n,
                    Some(p) => {
                        i += p;
                        if need(i, 1) {
                            return self.wait(i);
                        }
                        if b.get(i + 1) == Some(&b'"') {
                            i += 2;
                        } else {
                            i += 1;
                            self.st = In::Code;
                        }
                    }
                },
                In::Dollar(tag) => match find(&b[i..], &tag) {
                    // The close may begin in the last bytes, so those are looked at again with more text.
                    None if !eof && n - i >= tag.len() => return self.wait(n - tag.len() + 1),
                    None if !eof => return self.wait(i),
                    None => i = n,
                    Some(p) => {
                        i += p + tag.len();
                        self.st = In::Code;
                    }
                },
                // A carriage return ends a comment on the server as well.
                In::Line => match b[i..].iter().position(|&c| c == b'\n' || c == b'\r') {
                    None => i = n,
                    Some(p) => {
                        i += p;
                        self.st = In::Code;
                    }
                },
                In::Block(depth) => {
                    let mut j = i;
                    let mut d = depth;
                    while j + 1 < n && d > 0 {
                        if b[j] == b'/' && b[j + 1] == b'*' {
                            d += 1;
                            j += 2;
                        } else if b[j] == b'*' && b[j + 1] == b'/' {
                            d -= 1;
                            j += 2;
                        } else {
                            j += 1;
                        }
                    }
                    if d == 0 {
                        self.st = In::Code;
                        i = j;
                    } else if eof {
                        i = n;
                    } else {
                        self.st = In::Block(d);
                        return self.wait(j);
                    }
                }
                In::Code => {
                    let c = b[i];
                    if c.is_ascii_whitespace() {
                        if !self.code && i == self.pos {
                            self.pos += 1;
                        }
                        i += 1;
                    } else if c == b';' && self.depth == 0 && self.atomic == 0 {
                        return match self.take(i, i + 1)? {
                            Some(p) => Ok(Some(p)),
                            None => {
                                i = self.scan;
                                continue;
                            }
                        };
                    } else if c == b'-' || c == b'/' {
                        if need(i, 1) {
                            return self.wait(i);
                        }
                        let nx = b.get(i + 1).copied();
                        if c == b'-' && nx == Some(b'-') {
                            self.st = In::Line;
                            i += 2;
                        } else if c == b'/' && nx == Some(b'*') {
                            self.st = In::Block(1);
                            i += 2;
                        } else {
                            self.code = true;
                            self.last.clear();
                            i += 1;
                        }
                    } else if c == b'\'' {
                        self.code = true;
                        self.last.clear();
                        self.st = In::Str(self.plain_backslash);
                        i += 1;
                    } else if c == b'"' {
                        self.code = true;
                        self.last.clear();
                        self.st = In::Quoted;
                        i += 1;
                    } else if c == b'\\' {
                        let end = b[i..].iter().position(|&x| x == b'\n').map_or(n, |p| i + p);
                        if end == n && !eof {
                            return self.wait(i);
                        }
                        let cmd: String = String::from_utf8_lossy(&b[i..end]).split_whitespace().next().unwrap_or("\\").chars().take(40).collect();
                        let line = self.line_at(i);
                        return Err(meta(line, &cmd));
                    } else if c == b'$' {
                        self.last.clear();
                        // $1 is a parameter; $tag$ or $$ opens a string only the same mark closes. A `$` glued to a word
                        // never gets here because the word swallows it.
                        let mut j = i + 1;
                        if b.get(j).is_some_and(|d| d.is_ascii_digit()) {
                            while j < n && b[j].is_ascii_digit() {
                                j += 1;
                            }
                            if j == n && !eof {
                                return self.wait(i);
                            }
                            self.code = true;
                            i = j;
                        } else {
                            while j < n && (ident_start(b[j]) || (j > i + 1 && b[j].is_ascii_digit())) {
                                j += 1;
                            }
                            if j >= n && !eof {
                                return self.wait(i);
                            }
                            self.code = true;
                            if b.get(j) == Some(&b'$') {
                                self.st = In::Dollar(b[i..=j].to_vec());
                                i = j + 1;
                            } else {
                                i += 1;
                            }
                        }
                    } else if ident_start(c) || c.is_ascii_digit() {
                        // A word, read whole so a prefix letter or a keyword is never cut by a chunk edge.
                        let mut j = i;
                        while j < n && ident_cont(b[j]) {
                            j += 1;
                        }
                        if j >= n && !eof {
                            return self.wait(i);
                        }
                        let word = &b[i..j];
                        let one = j - i == 1;
                        let q = b.get(j).copied();
                        if one && q == Some(b'\'') && matches!(c, b'e' | b'E') {
                            self.code = true;
                            self.last.clear();
                            self.st = In::Str(true);
                            i = j + 1;
                        } else if one && q == Some(b'\'') && matches!(c, b'b' | b'B' | b'x' | b'X') {
                            // The server reads bit and hex strings with no escapes at all, whatever standard_conforming_strings is.
                            self.code = true;
                            self.last.clear();
                            self.st = In::Str(false);
                            i = j + 1;
                        } else if one && q == Some(b'\'') && matches!(c, b'n' | b'N') {
                            // A national string is N followed by an ordinary string, so it follows the session's escaping.
                            self.code = true;
                            self.last.clear();
                            self.st = In::Str(self.plain_backslash);
                            i = j + 1;
                        } else if one && matches!(c, b'u' | b'U') && q == Some(b'&') && matches!(b.get(j + 1), Some(b'\'' | b'"')) {
                            // The backslash of a Unicode escape string is not a string escape.
                            self.code = true;
                            self.last.clear();
                            self.st = if b[j + 1] == b'"' { In::Quoted } else { In::Str(false) };
                            i = j + 2;
                        } else {
                            let w = String::from_utf8_lossy(word).to_ascii_lowercase();
                            self.code = true;
                            self.word(&w);
                            i = j;
                        }
                    } else {
                        self.code = true;
                        self.last.clear();
                        if c == b'(' {
                            self.depth += 1;
                        } else if c == b')' {
                            self.depth = self.depth.saturating_sub(1);
                        }
                        i += 1;
                    }
                }
            }
            if !was_code && self.code {
                self.code_at = at;
            }
        }
    }

    /// A word of the statement's code: what the statement is, and where a SQL-standard function body opens and ends.
    fn word(&mut self, w: &str) {
        // Only words that follow each other at depth 0 make BEGIN ATOMIC, as psql reads it: any other token or an open
        // parenthesis in between (a function called `begin` with an `atomic` argument) breaks the pair.
        if self.depth > 0 {
            self.last.clear();
        }
        if self.head.len() < 6 && self.depth == 0 {
            self.head.push(w.to_string());
            let kind = self.head.iter().skip(1).find(|x| !matches!(x.as_str(), "or" | "replace")).map(String::as_str);
            self.routine = self.head.first().is_some_and(|f| f == "create") && matches!(kind, Some("function" | "procedure"));
        }
        if self.routine {
            if self.atomic == 0 && self.depth == 0 && w == "atomic" && self.last == "begin" {
                self.atomic = 1;
            } else if self.atomic > 0 {
                if w == "case" {
                    self.atomic += 1;
                } else if w == "end" {
                    self.atomic -= 1;
                }
            }
        }
        if self.depth == 0 {
            self.last = w.to_string();
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn meta(line: u64, cmd: &str) -> String {
    format!("Line {line}: {cmd} is a psql command, not SQL, and Quarry runs SQL only. Remove the line (a \\connect or \\c line needs the connection picked in Quarry instead), or run the file with psql.")
}

/// Refuses a statement that only psql can run. The data lines of a COPY FROM stdin follow the statement in the file and
/// would be read as SQL, so the run stops at the COPY itself.
fn refuse(stmt: &str, line: u64) -> Result<(), String> {
    use crate::db::sql::lex::Tok;
    let words: Vec<String> = crate::db::sql::lex::tokens(stmt, crate::db::sql::Dialect::Postgres).into_iter().filter(|(t, _)| matches!(t, Tok::Word | Tok::Punct | Tok::Quoted)).map(|(_, r)| stmt[r].to_ascii_lowercase()).collect();
    if words.first().is_some_and(|w| w == "copy") {
        // A parenthesised query is `COPY (SELECT ..) TO`, which has no FROM at the top level; its FROM is inside
        // the parentheses, so only a FROM at depth 0 counts.
        let mut depth = 0i32;
        for (i, w) in words.iter().enumerate() {
            match w.as_str() {
                "(" => depth += 1,
                ")" => depth -= 1,
                "from" if depth == 0 && words.get(i + 1).is_some_and(|n| n == "stdin") => {
                    return Err(format!("Line {line}: COPY ... FROM stdin has its data lines in the file, which Quarry cannot send. Dump with INSERT statements (a Quarry dump or pg_dump --inserts), or run the file with psql."));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(s: &str, chunk: usize) -> Result<Vec<String>, String> {
        let mut sp = PgSplitter::new(false);
        let mut out = Vec::new();
        for part in s.as_bytes().chunks(chunk.max(1)) {
            sp.feed(part);
            while let Some(p) = sp.next_piece()? {
                out.push(p.text);
            }
        }
        sp.finish();
        while let Some(p) = sp.next_piece()? {
            out.push(p.text);
        }
        Ok(out)
    }

    /// Every test text is split whole and in chunks of every small size, so a mark cut by a chunk edge is covered.
    fn same(s: &str) -> Vec<String> {
        let whole = all(s, usize::MAX).unwrap();
        for k in 1..9 {
            assert_eq!(all(s, k).unwrap(), whole, "chunks of {k} for {s:?}");
        }
        whole
    }

    #[test]
    fn a_body_with_semicolons_stays_one_statement() {
        assert_eq!(same("SELECT 1; SELECT 2;"), ["SELECT 1", "SELECT 2"]);
        assert_eq!(same("CREATE FUNCTION f() RETURNS int AS $$ BEGIN PERFORM 1; RETURN 2; END $$ LANGUAGE plpgsql; SELECT 3"), ["CREATE FUNCTION f() RETURNS int AS $$ BEGIN PERFORM 1; RETURN 2; END $$ LANGUAGE plpgsql", "SELECT 3"]);
        assert_eq!(same("SELECT $tag$ a; $$ b; $tag$; SELECT 2"), ["SELECT $tag$ a; $$ b; $tag$", "SELECT 2"]);
        // A different tag, or the same tag in another case, does not close.
        assert_eq!(same("SELECT $A$ x; $a$ y; $A$; SELECT 2"), ["SELECT $A$ x; $a$ y; $A$", "SELECT 2"]);
        // A `$` glued to a word opens nothing, and $1 is a parameter.
        assert_eq!(same("SELECT a$b$c; SELECT $1; SELECT 2"), ["SELECT a$b$c", "SELECT $1", "SELECT 2"]);
        assert_eq!(same("SELECT $$$$; SELECT 2"), ["SELECT $$$$", "SELECT 2"]);
    }

    #[test]
    fn strings_comments_and_names_hide_a_semicolon() {
        assert_eq!(same("SELECT 'a;b', \"c;d\"; SELECT 2"), ["SELECT 'a;b', \"c;d\"", "SELECT 2"]);
        assert_eq!(same("SELECT 1 -- x; y\n; SELECT 2"), ["SELECT 1 -- x; y", "SELECT 2"]);
        assert_eq!(same("SELECT /* a /* b; */ c; */ 1; SELECT 2"), ["SELECT /* a /* b; */ c; */ 1", "SELECT 2"]);
        assert_eq!(same(";; ;\n-- only a comment\n;/* c */;"), Vec::<String>::new());
        // E'' honours backslashes, a plain string does not.
        assert_eq!(same(r"SELECT E'it\'s; x'; SELECT 2"), [r"SELECT E'it\'s; x'", "SELECT 2"]);
        assert_eq!(same(r"SELECT 'a\'; SELECT 2"), [r"SELECT 'a\'", "SELECT 2"]);
        assert_eq!(same("SELECT 'it''s; x'; SELECT 2"), ["SELECT 'it''s; x'", "SELECT 2"]);
        assert_eq!(same(r"SELECT U&'a\0041;b'; SELECT 2"), [r"SELECT U&'a\0041;b'", "SELECT 2"]);
        assert_eq!(same(r#"SELECT "a;""b"; SELECT 2"#), [r#"SELECT "a;""b""#, "SELECT 2"]);
        // A statement of only comments is not sent, and a last statement needs no `;`.
        assert_eq!(same("-- a\n/* b */ SELECT 1"), ["-- a\n/* b */ SELECT 1"]);
    }

    #[test]
    fn plain_strings_follow_standard_conforming_strings() {
        let mut sp = PgSplitter::new(true);
        sp.feed(b"SELECT 'a\\'; x'; SELECT 2");
        sp.finish();
        assert_eq!(sp.next_piece().unwrap().unwrap().text, "SELECT 'a\\'; x'");
        assert_eq!(sp.next_piece().unwrap().unwrap().text, "SELECT 2");
    }

    #[test]
    fn parentheses_and_sql_bodies_hold_semicolons() {
        assert_eq!(same("CREATE RULE r AS ON INSERT TO t DO ALSO (INSERT INTO a VALUES (1); INSERT INTO b VALUES (2)); SELECT 2"), ["CREATE RULE r AS ON INSERT TO t DO ALSO (INSERT INTO a VALUES (1); INSERT INTO b VALUES (2))", "SELECT 2"]);
        let f = "CREATE FUNCTION f(a int) RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT CASE WHEN a > 1 THEN 1 ELSE 2 END; SELECT a; END; SELECT 2";
        assert_eq!(same(f), ["CREATE FUNCTION f(a int) RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT CASE WHEN a > 1 THEN 1 ELSE 2 END; SELECT a; END", "SELECT 2"]);
        // A plain BEGIN is a transaction statement and ends at its semicolon.
        assert_eq!(same("BEGIN; SELECT 1; COMMIT"), ["BEGIN", "SELECT 1", "COMMIT"]);
        assert_eq!(same("CREATE OR REPLACE PROCEDURE p() LANGUAGE sql BEGIN ATOMIC SELECT 1; END; SELECT 2"), ["CREATE OR REPLACE PROCEDURE p() LANGUAGE sql BEGIN ATOMIC SELECT 1; END", "SELECT 2"]);
    }

    #[test]
    fn begin_atomic_is_two_words_in_a_row_at_the_top_level() {
        // A function called begin with an argument called atomic is no SQL body: its semicolon ends the statement.
        assert_eq!(same("CREATE FUNCTION begin(atomic int) RETURNS int LANGUAGE sql AS 'select 1'; DROP TABLE v1; END;"), ["CREATE FUNCTION begin(atomic int) RETURNS int LANGUAGE sql AS 'select 1'", "DROP TABLE v1", "END"]);
        // Something between the two words, or a parenthesis open, breaks the pair as well.
        assert_eq!(same("CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'x' BEGIN, atomic; SELECT 2"), ["CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'x' BEGIN, atomic", "SELECT 2"]);
        assert_eq!(same("CREATE FUNCTION f(a begin, b (atomic)) RETURNS int LANGUAGE sql AS 'x'; SELECT 2"), ["CREATE FUNCTION f(a begin, b (atomic)) RETURNS int LANGUAGE sql AS 'x'", "SELECT 2"]);
        // A comment between them is not a token, so the pair stands.
        assert_eq!(same("CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN /* c */ ATOMIC SELECT 1; END; SELECT 2"), ["CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN /* c */ ATOMIC SELECT 1; END", "SELECT 2"]);
    }

    #[test]
    fn bit_and_hex_strings_never_escape_and_national_ones_follow_the_setting() {
        // The server reads no backslash escape inside B'' or X'', whatever standard_conforming_strings is.
        for plain in [false, true] {
            let mut sp = PgSplitter::new(plain);
            sp.feed(b"SELECT B'1\\'; SELECT 2; SELECT X'0\\'; SELECT 3");
            sp.finish();
            let got: Vec<String> = std::iter::from_fn(|| sp.next_piece().unwrap()).map(|p| p.text).collect();
            assert_eq!(got, ["SELECT B'1\\'", "SELECT 2", "SELECT X'0\\'", "SELECT 3"], "scs off: {plain}");
        }
        let mut sp = PgSplitter::new(true);
        sp.feed(b"SELECT N'a\\'; x'; SELECT 2");
        sp.finish();
        assert_eq!(sp.next_piece().unwrap().unwrap().text, "SELECT N'a\\'; x'");
    }

    #[test]
    fn psql_commands_and_copy_from_stdin_are_refused_with_their_line() {
        let e = all("SELECT 1;\n\\connect other\nSELECT 2", usize::MAX).unwrap_err();
        assert!(e.starts_with("Line 2:") && e.contains("\\connect") && e.contains("psql"), "{e}");
        let e = all("SELECT 1;\n\n\nCOPY t (a, b) FROM stdin;\n1\t2\n\\.\n", usize::MAX).unwrap_err();
        assert!(e.starts_with("Line 4:") && e.contains("COPY ... FROM stdin"), "{e}");
        for t in ["copy t from STDIN with (format csv)", "COPY \"t\" FROM\nstdin", "COPY t(a) FROM stdin"] {
            assert!(all(t, usize::MAX).is_err(), "{t}");
        }
        // Reading a query out, or a string that says stdin, is not a refusal.
        assert_eq!(all("COPY (SELECT 1 FROM stdin_t) TO stdout; SELECT 'COPY t FROM stdin'", usize::MAX).unwrap().len(), 2);
        let e = all("SELECT 1 \\gexec", usize::MAX).unwrap_err();
        assert!(e.contains("\\gexec"), "{e}");
        // A backslash inside a string or a comment is text.
        assert_eq!(all("SELECT E'\\\\n' -- \\q\n; SELECT '\\d'", usize::MAX).unwrap().len(), 2);
    }

    #[test]
    fn lines_count_across_chunks_and_a_runaway_quote_is_an_error() {
        let mut sp = PgSplitter::new(false);
        sp.feed(b"SELECT 1;\n\n-- c\n");
        let p = sp.next_piece().unwrap().unwrap();
        assert_eq!(p.line, 1);
        assert_eq!(sp.next_piece().unwrap(), None);
        sp.feed(b"SELECT 2;\nSELECT 3");
        assert_eq!(sp.next_piece().unwrap().unwrap().line, 4);
        sp.finish();
        assert_eq!(sp.next_piece().unwrap().unwrap().line, 5);
        let mut sp = PgSplitter::new(false);
        sp.set_max(1 << 20);
        sp.feed(b"SELECT '");
        sp.feed(&vec![b'x'; 2 << 20]);
        let e = sp.next_piece().unwrap_err();
        assert!(e.starts_with("Line 1:") && e.contains("left open"), "{e}");
    }

    #[test]
    fn text_that_is_not_utf8_is_refused() {
        let mut sp = PgSplitter::new(false);
        sp.feed(b"SELECT '\xff';");
        let e = sp.next_piece().unwrap_err();
        assert!(e.contains("not valid UTF-8"), "{e}");
    }
}
