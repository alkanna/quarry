//! The shell script side of SQL: a connection kept for a whole run, and values filled into its statements.

use super::run::{kill_query, prepare};
use super::worker::{make_cols, raw};
use super::{lex, value, Col, Db};
use crate::db::{e, Res};
use mongodb::bson::{Bson, Document};
use sqlx::{raw_sql, Either, Executor, Row as _};
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

/// A literal for one bound value. The script's text goes through the text protocol so rows convert
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
    let (at, count) = lex::placeholders(text, backslash, ansi);
    if lex::placeholders(text, !backslash, ansi) != (at, count) {
        return Err("This text reads differently with and without backslash escapes. Use ? for values and keep backslashes out of quotes".into());
    }
    let (mut out, mut last, mut n) = (String::new(), 0, 0);
    for (t, r) in lex::tokens_with(text, backslash, ansi) {
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

/// A shell script's own connection, kept for the whole run so its @variables and transactions persist.
/// It is the only way a script reaches the server, so the read-only check below covers every write.
pub struct Relay {
    conn: sqlx::MySqlConnection,
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
        let mut conn = db.acquire().await?.detach();
        prepare(&mut conn, db.dialect, schema, read_only, id).await.map_err(e)?;
        // Values are filled in for this session's mode, which may differ from the one read at connect.
        let m = super::ddl::session_mode(&mut conn).await?;
        Ok(Relay { conn, read_only, backslash: m.bs, ansi: m.ansi, dead: false, log_conn: db.conn, schema: schema.to_string() })
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
        if lex::changes_escaping(text) {
            return Err("This statement changes how values are escaped; not allowed in the shell".into());
        }
        if lex::uses_prepared(text) {
            return Err("Prepared statements are not available in the shell; use sql`...` with values instead.".into());
        }
        let stmt = fill(text, params, self.backslash, self.ansi)?;
        if lex::count_with(&stmt, self.backslash, self.ansi) != 1 || lex::count_with(&stmt, !self.backslash, self.ansi) != 1 {
            return Err("Send one statement per query".into());
        }
        if self.read_only {
            lex::read_only_check(&stmt)?;
        }
        let (mut cols, mut rows, mut wrote, mut bytes) = (Vec::<Col>::new(), Vec::new(), None, 0usize);
        let mut failure = None;
        let started = Instant::now();
        {
            let mut s = raw_sql(&stmt).fetch_many(&mut self.conn);
            while let Some(item) = futures_util::StreamExt::next(&mut s).await {
                match item {
                    Ok(Either::Right(row)) => {
                        if cols.is_empty() {
                            cols = make_cols(row.columns(), &HashMap::new(), false);
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
                    Ok(Either::Left(r)) if cols.is_empty() && wrote.is_none() => wrote = Some((r.rows_affected(), r.last_insert_id())),
                    Ok(Either::Left(_)) => {}
                    Err(err) => {
                        failure = Some(e(err));
                        break;
                    }
                }
            }
        }
        crate::db::log_sql(self.log_conn, &self.schema, &stmt, started.elapsed().as_millis(), failure.as_deref());
        if let (None, Some(to)) = (&failure, lex::use_target(&stmt)) {
            self.schema = to;
        }
        if let Some(f) = failure {
            // Dropping the stream mid-result leaves unread packets on the wire.
            self.dead = f.contains("more than");
            return Err(f);
        }
        if cols.is_empty() {
            // An empty SELECT still has headers, a write or SET does not.
            let empty_read = wrote.is_some_and(|w| w.0 == 0) && (&mut self.conn).describe(&stmt).await.is_ok_and(|d| !d.columns().is_empty());
            if !empty_read {
                let (affected, insert_id) = wrote.unwrap_or_default();
                return Ok(Answer::Wrote { affected, insert_id });
            }
        }
        Ok(Answer::Rows(rows))
    }
}
