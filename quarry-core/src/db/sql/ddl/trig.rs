//! Triggers: their definitions, the body check, ordering and the statements that save or drop one.

use super::*;

/// A trigger as the editor holds it. The body is the user's own SQL, written after FOR EACH ROW as typed. On PostgreSQL the
/// body is the source of the trigger's function, the event is the events joined with OR, and `pg` holds the rest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrigDef {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub body: String,
    pub pg: PgDef,
}

/// What a PostgreSQL trigger has that MySQL's does not.
#[derive(Clone, Debug, PartialEq)]
pub struct PgDef {
    /// FOR EACH ROW, else FOR EACH STATEMENT.
    pub per_row: bool,
    /// The UPDATE OF columns as typed, separated by commas; empty for any column.
    pub of_cols: String,
    /// The WHEN condition without its parentheses.
    pub when: String,
    /// tgenabled: O enabled, D disabled, R replica only, A always.
    pub mode: String,
    /// The name of the function, which a new trigger gets made; empty lets `pg::ddl_trig::func_name` pick one.
    pub func: String,
}

impl Default for PgDef {
    fn default() -> Self {
        PgDef { per_row: true, of_cols: String::new(), when: String::new(), mode: "O".into(), func: String::new() }
    }
}

/// The blocks a trigger body nests. A CASE inside an expression ends at a bare END, a CASE statement at END CASE.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Blk {
    Begin,
    If,
    CaseStmt,
    CaseExpr,
    Loop,
    While,
    Repeat,
    For,
}

impl Blk {
    /// The word after END that closes this block, or None for a bare END.
    fn end_word(self) -> Option<&'static str> {
        match self {
            Blk::Begin | Blk::CaseExpr => None,
            Blk::If => Some("IF"),
            Blk::CaseStmt => Some("CASE"),
            Blk::Loop => Some("LOOP"),
            Blk::While => Some("WHILE"),
            Blk::Repeat => Some("REPEAT"),
            Blk::For => Some("FOR"),
        }
    }
}

/// The body without its trailing `;`, refused unless the server reads all of it as the one statement after FOR EACH
/// ROW. The text goes over the text protocol, where CREATE TRIGGER has to (it cannot be prepared) and where a second
/// statement would run too, so the check follows the server's grammar: a body is one plain statement, or one
/// compound statement (BEGIN, IF, CASE, LOOP, WHILE, REPEAT or MariaDB's FOR, maybe labelled) whose blocks are
/// tracked to the END that closes it. Only a `;` may follow that END. When the check cannot tell what the server
/// would read it refuses rather than guesses: it may see a block close earlier than the server (and refuse what
/// follows), never later, so a block opens here only where the grammar opens one and every END closes one.
/// Strings and comments are read with the session's quote rules, and executable comments are refused because the
/// server runs or skips them by version.
pub fn check_body(body: &str, m: Mode) -> Result<String, String> {
    use crate::db::sql::lex::{tokens_with, Tok};
    let toks: Vec<(Tok, std::ops::Range<usize>)> = tokens_with(body, crate::db::sql::Dialect::MySql, m.bs, m.ansi).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    if toks.iter().any(|(t, _)| matches!(t, Tok::ExecOpen | Tok::ExecClose)) {
        return Err("A trigger body cannot hold an executable comment (/*! ... */): whether the server runs it depends on its version.".into());
    }
    let Some(first) = toks.first() else { return Err("The trigger body is empty.".into()) };
    let text = |k: usize| &body[toks[k].1.clone()];
    let word = |k: usize, w: &str| k < toks.len() && toks[k].0 == Tok::Word && text(k).eq_ignore_ascii_case(w);
    let punct = |k: usize, p: &str| k < toks.len() && toks[k].0 == Tok::Punct && text(k) == p;
    // A reserved word after a dot is a name (NEW.end, NEW.case), never a keyword.
    let after_dot = |k: usize| k > 0 && punct(k - 1, ".");
    let unquote = |s: &str| if s.len() >= 2 && (s.starts_with('`') || s.starts_with('"')) { s[1..s.len() - 1].to_string() } else { s.to_string() };
    let several = "A body of several statements goes inside BEGIN ... END, and nothing may follow its END but a ;.";
    let mut stack: Vec<(Blk, Option<String>)> = Vec::new();
    // At the first word of a statement, where BEGIN, IF and the rest open blocks.
    let mut start = true;
    // The statement after FOR EACH ROW is over: a `;` ended a plain one or an END closed the outermost block.
    let mut closed = false;
    let mut semi = false;
    let mut end = first.1.start;
    let mut k = 0;
    while k < toks.len() {
        let (t, r) = (toks[k].0, toks[k].1.clone());
        if closed {
            if punct(k, ";") && !semi {
                semi = true;
                k += 1;
                continue;
            }
            return Err(several.into());
        }
        if punct(k, ";") {
            match stack.last() {
                None => (closed, semi) = (true, true),
                Some((Blk::CaseExpr, _)) => return Err("A CASE expression is not closed before the ;.".into()),
                Some(_) => start = true,
            }
            k += 1;
            continue;
        }
        end = r.end;
        let is_word = t == Tok::Word && !after_dot(k);
        let up = if is_word { text(k).to_ascii_uppercase() } else { String::new() };
        if start {
            start = false;
            let mut label = None;
            let mut j = k;
            if matches!(t, Tok::Word | Tok::Quoted) && punct(k + 1, ":") {
                label = Some(unquote(text(k)).to_lowercase());
                j = k + 2;
            }
            let opener = match () {
                _ if word(j, "BEGIN") => Some(Blk::Begin),
                _ if word(j, "IF") && label.is_none() => Some(Blk::If),
                _ if word(j, "CASE") && label.is_none() => Some(Blk::CaseStmt),
                _ if word(j, "LOOP") => Some(Blk::Loop),
                _ if word(j, "WHILE") => Some(Blk::While),
                _ if word(j, "REPEAT") => Some(Blk::Repeat),
                _ if word(j, "FOR") => Some(Blk::For),
                _ => None,
            };
            if let Some(b) = opener {
                stack.push((b, label));
                // A block of statements starts one at once; IF, CASE, WHILE and FOR have a condition first.
                start = matches!(b, Blk::Begin | Blk::Loop | Blk::Repeat);
                end = toks[j].1.end;
                k = j + 1;
                continue;
            }
            if label.is_some() {
                return Err("A label goes before BEGIN, LOOP, WHILE or REPEAT.".into());
            }
            // DECLARE ... HANDLER FOR conditions: the handler's own statement starts after the conditions.
            if word(k, "DECLARE") {
                let mut j = k + 1;
                while j < toks.len() && !punct(j, ";") && !word(j, "HANDLER") {
                    j += 1;
                }
                if word(j, "HANDLER") && word(j + 1, "FOR") {
                    j += 2;
                    loop {
                        if word(j, "SQLSTATE") {
                            j += 1 + usize::from(word(j + 1, "VALUE"));
                            j += 1;
                        } else if word(j, "NOT") {
                            j += 2;
                        } else {
                            j += 1;
                        }
                        if punct(j, ",") {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    if j <= toks.len() {
                        end = toks[j - 1].1.end;
                        start = true;
                        k = j;
                        continue;
                    }
                }
            }
        }
        if is_word && up == "CASE" {
            stack.push((Blk::CaseExpr, None));
        } else if is_word && up == "END" {
            let Some((top, label)) = stack.pop() else { return Err(several.into()) };
            let next_kw = ["IF", "CASE", "LOOP", "WHILE", "REPEAT", "FOR"].into_iter().find(|w| word(k + 1, w));
            if next_kw != top.end_word() {
                let want = top.end_word().map_or("END".to_string(), |w| format!("END {w}"));
                return Err(format!("The blocks of the body do not match: {} where {want} was expected.", next_kw.map_or("END".to_string(), |w| format!("END {w}"))));
            }
            k += 1 + usize::from(next_kw.is_some());
            end = toks[k - 1].1.end;
            if top != Blk::CaseExpr {
                // An end label must repeat the begin label, so only that word is taken as one.
                if let Some(l) = &label && k < toks.len() && matches!(toks[k].0, Tok::Word | Tok::Quoted) && unquote(text(k)).to_lowercase() == *l {
                    end = toks[k].1.end;
                    k += 1;
                }
                closed = stack.is_empty();
            }
            continue;
        } else if is_word && ((matches!(up.as_str(), "THEN" | "ELSE") && matches!(stack.last(), Some((Blk::If | Blk::CaseStmt, _)))) || (up == "DO" && matches!(stack.last(), Some((Blk::While | Blk::For, _))))) {
            start = true;
        }
        k += 1;
    }
    if let Some((b, _)) = stack.last() {
        return Err(format!("The body has {} with no {} to close it.", match b {
            Blk::Begin => "a BEGIN",
            Blk::If => "an IF",
            Blk::CaseStmt | Blk::CaseExpr => "a CASE",
            Blk::Loop => "a LOOP",
            Blk::While => "a WHILE",
            Blk::Repeat => "a REPEAT",
            Blk::For => "a FOR",
        }, b.end_word().map_or("END".to_string(), |w| format!("END {w}"))));
    }
    let out = body[first.1.start..end].trim();
    if out.is_empty() || out == ";" {
        return Err("The trigger body is empty.".into());
    }
    Ok(out.to_string())
}

/// Where a trigger fires among the triggers of its table with the same timing and event.
#[derive(Clone, Debug, PartialEq)]
pub enum TrigOrder {
    Follows(String),
    Precedes(String),
}

/// The triggers just before and just after `name` in its timing and event, by ACTION_ORDER.
pub fn neighbours(all: &[Trigger], name: &str) -> (Option<String>, Option<String>) {
    let Some(me) = all.iter().find(|t| same_name(&t.name, name)) else { return (None, None) };
    let group = all.iter().filter(|t| !same_name(&t.name, name) && t.timing.eq_ignore_ascii_case(&me.timing) && t.event.eq_ignore_ascii_case(&me.event));
    let before = group.clone().filter(|t| t.order < me.order).max_by_key(|t| t.order).map(|t| t.name.clone());
    let after = group.filter(|t| t.order > me.order).min_by_key(|t| t.order).map(|t| t.name.clone());
    (before, after)
}

/// The clause that puts a trigger back between its old neighbours, among the triggers `live` that are there now:
/// after the one before it, else before the one after it, else none (it is the only one, or both are gone).
pub(in crate::db::sql) fn order_among(live: &[Trigger], name: &str, timing: &str, event: &str, before: Option<&str>, after: Option<&str>) -> Option<TrigOrder> {
    let here = |n: &str| live.iter().any(|t| !same_name(&t.name, name) && same_name(&t.name, n) && t.timing.eq_ignore_ascii_case(timing) && t.event.eq_ignore_ascii_case(event));
    match (before, after) {
        (Some(b), _) if here(b) => Some(TrigOrder::Follows(b.to_string())),
        (_, Some(a)) if here(a) => Some(TrigOrder::Precedes(a.to_string())),
        _ => None,
    }
}

pub(super) fn order_clause(d: Dialect, o: Option<&TrigOrder>) -> String {
    match o {
        Some(TrigOrder::Follows(n)) => format!(" FOLLOWS {}", d.quote_ident(n)),
        Some(TrigOrder::Precedes(n)) => format!(" PRECEDES {}", d.quote_ident(n)),
        None => String::new(),
    }
}

/// `DEFINER=`user`@`host`` from information_schema's `user@host`. The host never holds an @, so the last one splits.
pub(in crate::db::sql) fn definer_clause(d: Dialect, definer: &str) -> Result<String, String> {
    let (user, host) = definer.rsplit_once('@').ok_or_else(|| format!("The definer {definer:?} cannot be written back."))?;
    Ok(format!("DEFINER={}@{}", d.quote_ident(user), d.quote_ident(host)))
}

/// How a trigger is saved: the editor's definition, the definer it keeps (an edited trigger stays its owner's) and
/// its place among the triggers of its timing and event.
#[derive(Clone, Debug, PartialEq)]
pub struct TrigSave {
    pub def: TrigDef,
    pub definer: Option<String>,
    pub order: Option<TrigOrder>,
}

/// The CREATE TRIGGER statement. `replace` writes MariaDB's CREATE OR REPLACE, which swaps an existing trigger in
/// one statement.
pub(in crate::db::sql) fn create_trigger(d: Dialect, m: Mode, table: &str, s: &TrigSave, replace: bool) -> Result<String, String> {
    let t = &s.def;
    check_ident(&t.name, "A trigger")?;
    let timing = t.timing.trim().to_ascii_uppercase();
    let event = t.event.trim().to_ascii_uppercase();
    if !matches!(timing.as_str(), "BEFORE" | "AFTER") {
        return Err("Timing is BEFORE or AFTER.".into());
    }
    if !matches!(event.as_str(), "INSERT" | "UPDATE" | "DELETE") {
        return Err("The event is INSERT, UPDATE or DELETE.".into());
    }
    let body = check_body(&t.body, m)?;
    let head = if replace { "CREATE OR REPLACE" } else { "CREATE" };
    let definer = match &s.definer {
        Some(x) => format!(" {}", definer_clause(d, x)?),
        None => String::new(),
    };
    Ok(format!("{head}{definer} TRIGGER {} {timing} {event} ON {}\nFOR EACH ROW{}\n{body}", d.quote_ident(&t.name), d.quote_ident(table), order_clause(d, s.order.as_ref())))
}

/// Checks a whole CREATE TRIGGER text again where it runs, under the session's own quote rules: the head up to FOR
/// EACH ROW and its order clause holds no `;` and no executable comment, and the rest is one body by `check_body`.
pub(in crate::db::sql) fn one_trigger_statement(sql: &str, m: Mode) -> Result<(), String> {
    use crate::db::sql::lex::{count_with, tokens_with, Tok};
    let toks: Vec<_> = tokens_with(sql, crate::db::sql::Dialect::MySql, m.bs, m.ansi).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    let w = |k: usize, s: &str| toks.get(k).is_some_and(|(t, r)| *t == Tok::Word && sql[r.clone()].eq_ignore_ascii_case(s));
    let at = (0..toks.len()).find(|&k| w(k, "FOR") && w(k + 1, "EACH") && w(k + 2, "ROW")).ok_or("The statement has no FOR EACH ROW.")?;
    let mut k = at + 3;
    if w(k, "FOLLOWS") || w(k, "PRECEDES") {
        k += 2;
    }
    let head = &sql[..toks.get(k).map_or(sql.len(), |(_, r)| r.start)];
    if toks[..k.min(toks.len())].iter().any(|(t, r)| matches!(t, Tok::ExecOpen | Tok::ExecClose) || (*t == Tok::Punct && &sql[r.clone()] == ";")) || count_with(head, crate::db::sql::Dialect::MySql, m.bs, m.ansi) != 1 {
        return Err("The trigger statement is not one statement.".into());
    }
    let body = toks.get(k).map_or("", |(_, r)| &sql[r.start..]);
    check_body(body, m).map(|_| ())
}

/// A stored trigger statement made ready to run again: MariaDB's OR REPLACE after CREATE, and the order clause after
/// FOR EACH ROW in place of one the text may already hold. The text is read with the quote rules of the mode it was
/// stored with.
pub(in crate::db::sql) fn restate(d: Dialect, stmt: &str, m: Mode, order: Option<&TrigOrder>, replace: bool) -> Result<String, String> {
    use crate::db::sql::lex::{tokens_with, Tok};
    let toks: Vec<_> = tokens_with(stmt, d, m.bs, m.ansi).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    let w = |k: usize, s: &str| toks.get(k).is_some_and(|(t, r)| *t == Tok::Word && stmt[r.clone()].eq_ignore_ascii_case(s));
    if !w(0, "CREATE") {
        return Err("The stored trigger does not start with CREATE.".into());
    }
    let at = (0..toks.len()).find(|&k| w(k, "FOR") && w(k + 1, "EACH") && w(k + 2, "ROW")).ok_or("The stored trigger has no FOR EACH ROW.")?;
    let row_end = toks[at + 2].1.end;
    let rest_at = if w(at + 3, "FOLLOWS") || w(at + 3, "PRECEDES") { toks.get(at + 4).map_or(stmt.len(), |(_, r)| r.end) } else { row_end };
    let create_end = toks[0].1.end;
    let or_replace = if replace && !w(1, "OR") { " OR REPLACE" } else { "" };
    Ok(format!("{}{or_replace}{}{}{}", &stmt[..create_end], &stmt[create_end..row_end], order_clause(d, order), &stmt[rest_at..]))
}

/// Whether two sql_mode values hold the same modes, in any order.
pub fn same_modes(a: &str, b: &str) -> bool {
    let set = |s: &str| {
        let mut v: Vec<String> = s.split(',').map(|p| p.trim().to_ascii_uppercase()).filter(|p| !p.is_empty()).collect();
        v.sort();
        v
    };
    set(a) == set(b)
}

/// Whether the live trigger is the one a change left or an editor saw: every part that decides what it does, its
/// owner and its place.
pub(in crate::db::sql) fn same_trigger(live: &Trigger, want: &Trigger) -> bool {
    same_name(&live.name, &want.name) && live.timing.eq_ignore_ascii_case(&want.timing) && live.event.eq_ignore_ascii_case(&want.event) && live.body.trim() == want.body.trim() && live.definer == want.definer && same_modes(&live.sql_mode, &want.sql_mode) && live.order == want.order
}

/// A trigger as the server stores it, enough to make it again exactly: SHOW CREATE TRIGGER's statement, the session
/// it was made in (its sql_mode decides how the body runs, and the character set how its strings read), and the
/// triggers it sat between.
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub stmt: String,
    pub sql_mode: String,
    pub charset: String,
    pub collation: String,
    pub timing: String,
    pub event: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// What a trigger change did, for its undo.
#[derive(Clone, Debug, PartialEq)]
pub struct TrigUndo {
    pub name: String,
    /// The trigger to put back, or None to drop the one the change created.
    pub restore: Option<Stored>,
    /// The trigger as the change left it, or None when the change dropped it.
    pub left: Option<Trigger>,
    pub label: String,
    /// PostgreSQL: the statements that put the trigger and its function back and the state they rely on. The two fields above
    /// are MySQL's and stay empty then.
    pub pg: Option<crate::db::sql::pg::ddl_trig::PgUndo>,
}

/// A trigger change for the worker.
#[derive(Clone, Debug)]
pub enum TrigJob {
    Create(TrigSave),
    /// `was` is the trigger as the editor read it; the change is refused when the server's differs.
    Edit { was: Trigger, save: TrigSave },
    Drop(Trigger),
}

/// What Save sends for the draft `def`: a new trigger, or the live one `was` replaced by it. An edited trigger keeps
/// its definer, and its place among the triggers of its timing and event when those stay the same.
pub fn save_job(all: &[Trigger], was: Option<&Trigger>, def: &TrigDef) -> TrigJob {
    let Some(was) = was else { return TrigJob::Create(TrigSave { def: def.clone(), definer: None, order: None }) };
    let kept = was.timing.eq_ignore_ascii_case(&def.timing) && was.event.eq_ignore_ascii_case(&def.event);
    let order = kept.then(|| neighbours(all, &was.name)).and_then(|(before, after)| before.map(TrigOrder::Follows).or(after.map(TrigOrder::Precedes)));
    let definer = (!was.definer.is_empty()).then(|| was.definer.clone());
    TrigJob::Edit { was: was.clone(), save: TrigSave { def: def.clone(), definer, order } }
}

/// The statements as the preview shows them. A MySQL edit also creates the new trigger under a scratch name first and
/// holds a write lock on the table while it swaps them, which the note under the preview says.
pub fn trigger_preview(d: Dialect, m: Mode, table: &str, job: &TrigJob) -> Result<Vec<String>, String> {
    Ok(match job {
        TrigJob::Create(s) => vec![create_trigger(d, m, table, s, false)?],
        TrigJob::Edit { save, .. } if matches!(d, Dialect::MariaDb) => vec![create_trigger(d, m, table, save, true)?],
        TrigJob::Edit { was, save } => vec![drop_trigger(d, &was.name, false), create_trigger(d, m, table, save, false)?],
        TrigJob::Drop(t) => vec![drop_trigger(d, &t.name, false)],
    })
}

pub fn drop_trigger(d: Dialect, name: &str, if_exists: bool) -> String {
    format!("DROP TRIGGER {}{}", if if_exists { "IF EXISTS " } else { "" }, d.quote_ident(name))
}

/// The statements as the preview shows them.
pub fn script(steps: &[String]) -> String {
    steps.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n\n")
}

/// The note for an error the server gives when the login may not create a trigger for another definer.
pub(super) fn definer_note(err: String, definer: Option<&str>) -> String {
    match definer {
        Some(who) if err.contains("1227") || err.contains("SET_USER_ID") || err.contains("SUPER") => format!("{err}. The trigger belongs to {who}, and keeping that owner needs the SET_USER_ID (or SUPER) privilege, which this login lacks. Nothing was changed."),
        _ => err,
    }
}
