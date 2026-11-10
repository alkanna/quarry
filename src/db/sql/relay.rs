//! The shell script side of SQL: a connection kept for a whole run, and values filled into its statements.

use super::exec::{Conn, Item};
use super::run::{kill_query, prepare};
use super::worker::{make_cols, raw};
use super::{lex, value, Col, Db, Dialect};
use crate::db::{e, Res};
use mongodb::bson::{Bson, Document};
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
/// The most rows one query of a shell script may return; past it the script gets an error instead of
/// the server's whole table through a pipe.
const RELAY_ROWS_MAX: usize = 100_000;
/// And the most bytes of values one answer may hold, so a few huge cells cannot fill memory or the pipe.
const RELAY_BYTES_MAX: usize = 64 << 20;

/// What a shell script's query gave back: rows as relaxed Extended JSON, or the effect of a write.
pub enum Answer {
    Rows(Vec<serde_json::Value>),
    Wrote { affected: u64, insert_id: u64 },
}

/// Where an unsafe JSON number would lose digits.
const JS_SAFE: u64 = (1 << 53) - 1;

/// A literal for one bound value. PostgreSQL has its own spelling, see `literal_pg`. The script's text goes through the text protocol so rows convert
/// exactly like the Custom Query's, so values are escaped here instead of bound.
fn literal(p: &value::Param, no_backslash: bool) -> Res<String> {
    use value::Param;
    Ok(match p {
        Param::Null => "NULL".into(),
        Param::Int(i) => i.to_string(),
        Param::UInt(u) => u.to_string(),
        Param::Float(f) if f.is_finite() => format!("{f:e}"),
        Param::Float(_) => return Err("A value is not a finite number".into()),
        Param::Text(s) => {
            if s.contains('\0') {
                return Err("A text value contains a NUL character".into());
            }
            // The backslash only escapes when the server reads it as an escape.
            value::lit(s, !no_backslash)
        }
        Param::Bytes(b) => format!("X'{}'", value::hex_digits(b, false)),
    })
}

/// A row cell as the script gets it. An Int64 beyond 2^53 is sent as {"$numberLong"} because a plain
/// JSON number would round in the script.
fn cell(b: Bson) -> Bson {
    match b {
        Bson::Int64(i) if i.unsigned_abs() > JS_SAFE => mongodb::bson::doc! { "$numberLong": i.to_string() }.into(),
        b => b,
    }
}

/// The text with every `?` outside strings and comments replaced by the next value's literal. The text is
/// read with the server's quote rules, and refused when the other backslash reading finds other
/// placeholders or statements, since the server could then see code where this sees a string.
pub fn fill(text: &str, params: &[value::Param], backslash: bool, ansi: bool) -> Res<String> {
    let (at, count) = lex::placeholders(text, Dialect::MySql, backslash, ansi);
    if lex::placeholders(text, Dialect::MySql, !backslash, ansi) != (at, count) {
        return Err("This text reads differently with and without backslash escapes. Use ? for values and keep backslashes out of quotes".into());
    }
    let (mut out, mut last, mut n) = (String::new(), 0, 0);
    for (t, r) in lex::tokens_with(text, Dialect::MySql, backslash, ansi) {
        if t == lex::Tok::Punct && &text[r.clone()] == "?" {
            let p = params.get(n).ok_or("The query has more ? than values")?;
            out.push_str(&text[last..r.start]);
            // Padded, so a literal never fuses with a word before or after it (X?, @?).
            out.push_str(&format!(" {} ", literal(p, !backslash)?));
            (last, n) = (r.end, n + 1);
        }
    }
    if n != params.len() {
        return Err(format!("The query has {n} placeholders but {} values", params.len()));
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// A literal for one bound value on PostgreSQL. A text with a backslash becomes an E string with every
/// backslash doubled, so it reads the same whatever standard_conforming_strings says.
fn literal_pg(p: &value::Param) -> Res<String> {
    use value::Param;
    Ok(match p {
        Param::Null => "NULL".into(),
        Param::Int(i) => i.to_string(),
        Param::UInt(u) => u.to_string(),
        Param::Float(f) if f.is_finite() => format!("{f:e}"),
        Param::Float(_) => return Err("A value is not a finite number".into()),
        Param::Text(s) => {
            if s.contains('\0') {
                return Err("A text value contains a NUL character".into());
            }
            let q = s.replace('\'', "''");
            if s.contains('\\') { format!("E'{}'", q.replace('\\', "\\\\")) } else { format!("'{q}'") }
        }
        // E'\\x..' holds one backslash, so this is bytea hex input in either reading of plain strings.
        Param::Bytes(b) => format!("E'\\\\x{}'::bytea", value::hex_digits(b, false)),
    })
}

/// The text with every `$n` outside strings, comments and dollar quotes replaced by the literal of value n. The
/// text is read with standard_conforming_strings on and off, and refused when the two find other parameters or
/// statements, since the server could then see code where this sees a string. Every value must be used.
pub fn fill_pg(text: &str, params: &[value::Param]) -> Res<String> {
    let d = Dialect::Postgres;
    let (at, count) = lex::placeholders(text, d, false, false);
    if lex::placeholders(text, d, true, false) != (at.clone(), count) {
        return Err("This text reads differently with and without standard_conforming_strings. Use $1 for values and keep backslashes out of quotes".into());
    }
    let (mut out, mut last, mut used) = (String::new(), 0, vec![false; params.len()]);
    for (t, r) in lex::tokens_with(text, d, false, false) {
        let Some(n) = (t == lex::Tok::Word).then(|| lex::param_number(&text[r.clone()])).flatten() else { continue };
        let p = n.checked_sub(1).and_then(|i| params.get(i)).ok_or_else(|| format!("The query uses ${n} but has {} values", params.len()))?;
        used[n - 1] = true;
        out.push_str(&text[last..r.start]);
        // Padded, so a literal never fuses with a word or an operator beside it.
        out.push_str(&format!(" {} ", literal_pg(p)?));
        last = r.end;
    }
    if let Some(i) = used.iter().position(|u| !u) {
        return Err(format!("The query never uses ${} but has {} values", i + 1, params.len()));
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// A shell script's own connection, kept for the whole run so its @variables and transactions persist.
/// It is the only way a script reaches the server, so the read-only check below covers every write.
pub struct Relay {
    conn: Conn,
    dialect: Dialect,
    read_only: bool,
    backslash: bool,
    ansi: bool,
    /// A result was abandoned halfway, which leaves the connection unusable.
    dead: bool,
    /// For the query log: the connection's app id and the database the script runs in.
    log_conn: u64,
    schema: String,
}

impl Relay {
    pub async fn open(db: &Db, schema: &str, read_only: bool, id: &AtomicU64) -> Res<Relay> {
        let mut conn = db.acquire_conn().await?;
        prepare(&mut conn, db.dialect, schema, read_only, id).await?;
        // Values are filled in for this session's mode, which may differ from the one read at connect.
        let m = super::ddl::session_mode(conn.my()?).await?;
        Ok(Relay { conn, dialect: db.dialect, read_only, backslash: m.bs, ansi: m.ansi, dead: false, log_conn: db.conn, schema: schema.to_string() })
    }

    /// Interrupts the statement running on connection `id`, from a connection of its own.
    pub async fn kill(db: &Db, id: u64) -> Res<String> {
        kill_query(db, id).await
    }

    /// Runs one statement. On a locked connection the same check as the Custom Query runs on the final
    /// text, and the session itself is READ ONLY.
    pub async fn query(&mut self, text: &str, params: &[value::Param]) -> Res<Answer> {
        if self.dead {
            return Err("The connection of this script was lost after an earlier error".into());
        }
        if lex::changes_escaping(text, self.dialect) {
            return Err("This statement changes how values are escaped; not allowed in the shell".into());
        }
        if lex::uses_prepared(text, self.dialect) {
            return Err("Prepared statements are not available in the shell; use sql`...` with values instead.".into());
        }
        let stmt = if self.dialect == Dialect::Postgres { fill_pg(text, params)? } else { fill(text, params, self.backslash, self.ansi)? };
        if lex::count_with(&stmt, self.dialect, self.backslash, self.ansi) != 1 || lex::count_with(&stmt, self.dialect, !self.backslash, self.ansi) != 1 {
            return Err("Send one statement per query".into());
        }
        if self.read_only {
            lex::read_only_check(&stmt, self.dialect)?;
        }
        let (mut cols, mut rows, mut wrote, mut bytes) = (Vec::<Col>::new(), Vec::new(), None, 0usize);
        let mut failure = None;
        let started = Instant::now();
        {
            let mut s = self.conn.fetch_text(&stmt);
            while let Some(item) = futures_util::StreamExt::next(&mut s).await {
                match item {
                    Ok(Item::Row(row)) => {
                        if cols.is_empty() {
                            cols = make_cols(&row.cols(), &HashMap::new(), false);
                        }
                        if rows.len() >= RELAY_ROWS_MAX {
                            failure = Some(format!("A query returned more than {RELAY_ROWS_MAX} rows. Add a LIMIT."));
                            break;
                        }
                        bytes += (0..cols.len()).map(|i| raw(&row, i).map_or(0, |b| b.len()) + 16).sum::<usize>();
                        if bytes > RELAY_BYTES_MAX {
                            failure = Some(format!("A query returned more than {} MB. Select fewer columns or add a LIMIT.", RELAY_BYTES_MAX >> 20));
                            break;
                        }
                        let d: Document = cols.iter().enumerate().map(|(i, c)| (c.name.clone(), cell(value::to_bson(c.kind, raw(&row, i))))).collect();
                        rows.push(Bson::Document(d).into_relaxed_extjson());
                    }
                    // A CALL has several results; the first one with rows is what the script gets.
                    Ok(Item::End(r)) if cols.is_empty() && wrote.is_none() => wrote = Some((r.rows_affected(), r.last_insert_id())),
                    Ok(Item::End(_)) => {}
                    Err(err) => {
                        failure = Some(e(err));
                        break;
                    }
                }
            }
        }
        crate::db::log_sql(self.log_conn, &self.schema, &stmt, started.elapsed().as_millis(), failure.as_deref());
        if let (None, Some(to)) = (&failure, lex::use_target(&stmt, self.dialect)) {
            self.schema = to;
        }
        if let Some(f) = failure {
            // Dropping the stream mid-result leaves unread packets on the wire.
            self.dead = f.contains("more than");
            return Err(f);
        }
        if cols.is_empty() {
            // An empty SELECT still has headers, a write or SET does not.
            let empty_read = wrote.is_some_and(|w| w.0 == 0) && self.conn.describe(&stmt).await.is_ok_and(|d| !d.is_empty());
            if !empty_read {
                let (affected, insert_id) = wrote.unwrap_or_default();
                return Ok(Answer::Wrote { affected, insert_id });
            }
        }
        Ok(Answer::Rows(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use value::Param;

    #[test]
    fn postgres_fill_replaces_parameters_outside_strings_comments_and_dollar_quotes() {
        let p = [Param::Int(7), Param::Text("it's".into())];
        assert_eq!(fill_pg("SELECT $1, $2::text", &p).unwrap(), "SELECT  7 ,  'it''s' ::text");
        assert_eq!(fill_pg("SELECT '$1', \"$1\", $$ $2 $$, /* $2 */ $1 -- $2\n, $2, $1", &p).unwrap(), "SELECT '$1', \"$1\", $$ $2 $$, /* $2 */  7  -- $2\n,  'it''s' ,  7 ");
        assert_eq!(fill_pg("SELECT $1 + $1, $2", &p).unwrap(), "SELECT  7  +  7 ,  'it''s' ");
        // A backslash needs an E string, which reads the same whatever standard_conforming_strings says.
        assert_eq!(fill_pg("SELECT $1", &[Param::Text(r"a\b'c".into())]).unwrap(), r"SELECT  E'a\\b''c' ");
        assert_eq!(fill_pg("SELECT $1, $2, $3, $4, $5", &[Param::Null, Param::UInt(u64::MAX), Param::Float(1.5), Param::Float(-2.0), Param::Bytes(vec![0, 255])]).unwrap(), r"SELECT  NULL ,  18446744073709551615 ,  1.5e0 ,  -2e0 ,  E'\\x00ff'::bytea ");
        // A value that tries to end its string stays inside it.
        let evil = fill_pg("SELECT $1", &[Param::Text("'; DROP TABLE t; --".into())]).unwrap();
        assert_eq!(lex::split_statements(&evil, Dialect::Postgres).len(), 1);
        assert!(lex::read_only_check(&evil, Dialect::Postgres).is_ok());
        let evil = fill_pg("SELECT $1", &[Param::Text(r"\'; DROP TABLE t; --".into())]).unwrap();
        assert_eq!(lex::split_statements(&evil, Dialect::Postgres).len(), 1);
        assert_eq!(lex::count_with(&evil, Dialect::Postgres, true, false), 1);
    }

    #[test]
    fn postgres_fill_refuses_what_it_cannot_read_one_way() {
        let p = [Param::Int(1)];
        assert!(fill_pg("SELECT $2", &p).is_err() && fill_pg("SELECT $0", &p).is_err());
        assert!(fill_pg("SELECT 1", &p).is_err());
        assert!(fill_pg("SELECT $1", &[Param::Float(f64::NAN)]).is_err());
        assert!(fill_pg("SELECT $1", &[Param::Text("a\0".into())]).is_err());
        // Without standard_conforming_strings the first string runs on and swallows the second parameter.
        assert!(fill_pg(r"SELECT '\' , $1 , '", &p).is_err());
    }
}
