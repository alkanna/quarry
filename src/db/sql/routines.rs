//! Functions and procedures (routines) of a MySQL, MariaDB or PostgreSQL database: the list, one routine's definition and
//! facts, the statements that save, drop and run one, and the MySQL sequence that cannot replace a routine in place.
//! Everything that builds text is plain and checked offline; the PostgreSQL reads and its transaction are in `pg::routines`.
//!
//! A routine is identified by its kind and name on MySQL and MariaDB, where a function and a procedure may share a name, and
//! by kind, name and identity arguments on PostgreSQL, where overloads share one. `Routine::key` is that identity.

use super::lex::{tokens, Tok};
use super::value::{lit, pg_lit};
use super::worker::{text, text_at};
use super::{se, Db, Dialect};
use crate::db::{Msg, Res, Worker};
use sqlx::Connection;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    Function,
    Procedure,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Function => "FUNCTION",
            Kind::Procedure => "PROCEDURE",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Procedure => "procedure",
        }
    }

    /// Reads information_schema's ROUTINE_TYPE and pg_proc's prokind letter.
    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_uppercase().as_str() {
            "FUNCTION" | "F" => Some(Kind::Function),
            "PROCEDURE" | "P" => Some(Kind::Procedure),
            _ => None,
        }
    }

    /// The letter pg_proc.prokind holds.
    pub fn letter(self) -> &'static str {
        match self {
            Kind::Function => "f",
            Kind::Procedure => "p",
        }
    }
}

/// One row of the sidebar list.
#[derive(Clone, Debug, PartialEq)]
pub struct Routine {
    pub kind: Kind,
    pub name: String,
    /// PostgreSQL: the identity arguments, `a integer, b text`, which tell overloads apart. Empty on MySQL.
    pub args: String,
    /// The return type; empty for a procedure.
    pub returns: String,
    /// `plpgsql`, `sql` or `c` on PostgreSQL; `SQL` or `EXTERNAL` on MySQL.
    pub lang: String,
}

impl Routine {
    /// What identifies the routine among those of its schema, for the list's keys and for finding it again after a reload.
    pub fn key(&self) -> String {
        format!("{}:{}({})", self.kind.word(), self.name, self.args)
    }

    /// The name as the list prints it: with its arguments on PostgreSQL, where they are part of the identity.
    pub fn label(&self, d: Dialect) -> String {
        if d == Dialect::Postgres { format!("{}({})", self.name, self.args) } else { self.name.clone() }
    }
}

/// The routines of a schema. Params: [schema]. Columns: name, kind, args, returns, lang.
/// PostgreSQL leaves out aggregates and window functions (prokind a and w) and what an extension installed, which a person
/// cannot edit or drop on its own.
pub fn list_sql(d: Dialect) -> &'static str {
    if d == Dialect::Postgres {
        return "SELECT p.proname::text AS name, p.prokind::text AS kind, pg_get_function_identity_arguments(p.oid) AS args, COALESCE(pg_get_function_result(p.oid), '') AS returns, l.lanname::text AS lang \
                FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace JOIN pg_language l ON l.oid = p.prolang \
                WHERE n.nspname = $1 AND p.prokind IN ('f', 'p') AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e') \
                ORDER BY p.proname, 3";
    }
    "SELECT ROUTINE_NAME AS name, ROUTINE_TYPE AS kind, '' AS args, COALESCE(DTD_IDENTIFIER, '') AS returns, COALESCE(ROUTINE_BODY, '') AS lang FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? AND ROUTINE_TYPE IN ('FUNCTION', 'PROCEDURE') ORDER BY ROUTINE_NAME, ROUTINE_TYPE"
}

/// A row of `list_sql`, read through `get` so a test needs no server.
pub fn routine_of(get: &dyn Fn(&str) -> Option<String>) -> Option<Routine> {
    Some(Routine { kind: Kind::parse(&get("kind")?)?, name: get("name")?, args: get("args").unwrap_or_default(), returns: get("returns").unwrap_or_default(), lang: get("lang").unwrap_or_default() })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgMode {
    In,
    Out,
    InOut,
    /// PostgreSQL VARIADIC: the call passes the array whole.
    Variadic,
}

impl ArgMode {
    /// information_schema's PARAMETER_MODE or pg_proc's proargmodes letter. None for a column of a RETURNS TABLE, which is
    /// part of the result and not an argument.
    pub fn parse(s: &str) -> Option<ArgMode> {
        match s.trim() {
            "" | "i" | "IN" => Some(ArgMode::In),
            "o" | "OUT" => Some(ArgMode::Out),
            "b" | "INOUT" => Some(ArgMode::InOut),
            "v" => Some(ArgMode::Variadic),
            _ => None,
        }
    }

    /// Whether the call takes a value for it.
    pub fn takes_value(self) -> bool {
        !matches!(self, ArgMode::Out)
    }

    pub fn label(self) -> &'static str {
        match self {
            ArgMode::In => "IN",
            ArgMode::Out => "OUT",
            ArgMode::InOut => "INOUT",
            ArgMode::Variadic => "VARIADIC",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub mode: ArgMode,
    pub ty: String,
}

/// What opening a routine shows and what an edit starts from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub routine: Option<Routine>,
    /// The whole CREATE statement. When `header_missing` is set it is only the body.
    pub source: String,
    /// Why the statement above the body could not be read (MySQL needs a privilege for SHOW CREATE), so the view is read-only.
    pub header_missing: Option<String>,
    /// The argument list as the server prints it.
    pub signature: String,
    pub returns: String,
    pub lang: String,
    /// DEFINER or INVOKER.
    pub security: String,
    /// MySQL: the account the routine runs as. PostgreSQL: the owner.
    pub owner: String,
    /// DETERMINISTIC or NOT DETERMINISTIC on MySQL; VOLATILE, STABLE or IMMUTABLE on PostgreSQL.
    pub behavior: String,
    /// MySQL: CONTAINS SQL, NO SQL, READS SQL DATA or MODIFIES SQL DATA.
    pub data_access: String,
    pub comment: String,
    pub params: Vec<Param>,
    /// PostgreSQL: the oid, which a replace keeps and a drop and create changes.
    pub oid: String,
    /// What the editor saw (definition, owner, privileges and comment), compared again before a change is written.
    pub seen: String,
    /// MySQL: the sql_mode the routine was created under.
    pub sql_mode: String,
    /// True for a PostgreSQL function declared STABLE or IMMUTABLE, which a locked connection may run: the session is read-only
    /// anyway, so a function that lies about it is refused by the server. Everything else counts as a write.
    pub read_only: bool,
}

/// PostgreSQL's detail row. Params: [schema, name, prokind, identity arguments].
pub const PG_DETAIL: &str = "SELECT p.oid::text AS oid, p.proname::text AS name, p.prokind::text AS kind, pg_get_function_identity_arguments(p.oid) AS args, pg_get_function_arguments(p.oid) AS signature, \
    COALESCE(pg_get_function_result(p.oid), '') AS returns, l.lanname::text AS lang, CASE WHEN p.prosecdef THEN 'DEFINER' ELSE 'INVOKER' END AS security, p.provolatile::text AS volatility, \
    pg_get_userbyid(p.proowner)::text AS owner, COALESCE(obj_description(p.oid, 'pg_proc'), '') AS comment, COALESCE(p.proacl::text, '') AS acl, pg_get_functiondef(p.oid) AS def \
    FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace JOIN pg_language l ON l.oid = p.prolang \
    WHERE n.nspname = $1 AND p.proname = $2 AND p.prokind::text = $3 AND pg_get_function_identity_arguments(p.oid) = $4";

/// The arguments of the PostgreSQL routine with this oid, in order. Params: [oid].
pub const PG_PARAMS: &str = "SELECT (a.n)::int::text AS pos, COALESCE((p.proargnames)[(a.n)::int], '') AS pname, COALESCE((p.proargmodes)[(a.n)::int]::text, 'i') AS mode, format_type(a.t, NULL) AS ty \
    FROM pg_proc p CROSS JOIN LATERAL unnest(COALESCE(p.proallargtypes, p.proargtypes::oid[])) WITH ORDINALITY AS a(t, n) WHERE p.oid = $1::oid ORDER BY a.n";

/// MySQL's detail row. Params: [schema, name, type].
pub const MY_DETAIL: &str = "SELECT ROUTINE_NAME AS name, ROUTINE_TYPE AS kind, COALESCE(DTD_IDENTIFIER, '') AS returns, COALESCE(ROUTINE_BODY, '') AS lang, COALESCE(SECURITY_TYPE, '') AS security, COALESCE(IS_DETERMINISTIC, '') AS deterministic, \
    COALESCE(SQL_DATA_ACCESS, '') AS access, COALESCE(ROUTINE_COMMENT, '') AS comment, COALESCE(DEFINER, '') AS definer, COALESCE(ROUTINE_DEFINITION, '') AS body, COALESCE(SQL_MODE, '') AS sql_mode \
    FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? AND ROUTINE_NAME = ? AND ROUTINE_TYPE = ?";

/// The parameters of a MySQL routine; position 0 is the return value of a function and is left out. Params: [schema, name, type].
pub const MY_PARAMS: &str = "SELECT ORDINAL_POSITION AS pos, COALESCE(PARAMETER_NAME, '') AS pname, COALESCE(PARAMETER_MODE, '') AS mode, COALESCE(DTD_IDENTIFIER, '') AS ty \
    FROM information_schema.PARAMETERS WHERE SPECIFIC_SCHEMA = ? AND SPECIFIC_NAME = ? AND ROUTINE_TYPE = ? AND ORDINAL_POSITION > 0 ORDER BY ORDINAL_POSITION";

/// One parameter row; None for a RETURNS TABLE column.
pub fn param_of(get: &dyn Fn(&str) -> Option<String>) -> Option<Param> {
    Some(Param { name: get("pname").unwrap_or_default(), mode: ArgMode::parse(&get("mode").unwrap_or_default())?, ty: get("ty").unwrap_or_default() })
}

/// The detail of a PostgreSQL routine from its row and its parameter rows.
pub fn pg_detail(get: &dyn Fn(&str) -> Option<String>, params: Vec<Param>) -> Option<Detail> {
    let g = |c: &str| get(c).unwrap_or_default();
    let routine = routine_of(get)?;
    let behavior = match g("volatility").as_str() {
        "i" => "IMMUTABLE",
        "s" => "STABLE",
        _ => "VOLATILE",
    };
    let def = g("def");
    let seen = [def.as_str(), &g("owner"), &g("acl"), &g("comment")].join("\u{1f}");
    Some(Detail {
        read_only: routine.kind == Kind::Function && behavior != "VOLATILE",
        routine: Some(routine),
        source: def,
        header_missing: None,
        signature: g("signature"),
        returns: g("returns"),
        lang: g("lang"),
        security: g("security"),
        owner: g("owner"),
        behavior: behavior.into(),
        data_access: String::new(),
        comment: g("comment"),
        params,
        oid: g("oid"),
        seen,
        sql_mode: String::new(),
    })
}

/// The detail of a MySQL or MariaDB routine. `create` is SHOW CREATE's statement, None when the account may not read it.
pub fn my_detail(get: &dyn Fn(&str) -> Option<String>, params: Vec<Param>, create: Option<String>) -> Option<Detail> {
    let g = |c: &str| get(c).unwrap_or_default();
    let routine = routine_of(get)?;
    let signature = params.iter().map(|p| [p.mode.label(), p.name.as_str(), p.ty.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join(", ");
    let (source, header_missing) = match create.filter(|c| !c.trim().is_empty()) {
        Some(c) => (c, None),
        None => (g("body"), Some("SHOW CREATE needs the SHOW_ROUTINE privilege, or to be the routine's definer, so only the body is shown and it cannot be edited here.".to_string())),
    };
    Some(Detail {
        returns: g("returns"),
        lang: g("lang"),
        security: g("security"),
        owner: g("definer"),
        behavior: if g("deterministic").eq_ignore_ascii_case("YES") { "DETERMINISTIC".into() } else { "NOT DETERMINISTIC".into() },
        data_access: g("access"),
        comment: g("comment"),
        sql_mode: g("sql_mode"),
        routine: Some(routine),
        source,
        header_missing,
        signature,
        params,
        oid: String::new(),
        seen: String::new(),
        read_only: false,
    })
}

// ---------- statements ----------

/// The DROP of a routine, with its schema, and on PostgreSQL its identity arguments.
pub fn drop_stmt(d: Dialect, schema: &str, r: &Routine) -> String {
    if d == Dialect::Postgres {
        return format!("DROP {} {}({})", r.kind.word(), d.qualified(schema, &r.name), r.args);
    }
    format!("DROP {} {}", r.kind.word(), d.qualified(schema, &r.name))
}

/// A starting point for a new routine, written for the dialect.
pub fn template(d: Dialect, kind: Kind, schema: &str) -> String {
    match (d, kind) {
        (Dialect::Postgres, Kind::Function) => format!("CREATE FUNCTION {}(a integer, b integer)\n RETURNS integer\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n  RETURN a + b;\nEND\n$function$\n", d.qualified(schema, "new_function")),
        (Dialect::Postgres, Kind::Procedure) => format!("CREATE PROCEDURE {}(a integer)\n LANGUAGE plpgsql\nAS $procedure$\nBEGIN\n  RAISE NOTICE 'a is %', a;\nEND\n$procedure$\n", d.qualified(schema, "new_procedure")),
        // DETERMINISTIC because a server that logs in binary mode refuses a function that declares nothing.
        (_, Kind::Function) => format!("CREATE FUNCTION {}(a INT)\nRETURNS INT\nDETERMINISTIC\nBEGIN\n  RETURN a + 1;\nEND", d.qualified(schema, "new_function")),
        (_, Kind::Procedure) => format!("CREATE PROCEDURE {}(IN a INT)\nBEGIN\n  SELECT a;\nEND", d.qualified(schema, "new_procedure")),
    }
}

/// What a CREATE statement says about itself: its kind and, when the lexer finds it, its name without the database.
pub fn head_of(d: Dialect, sql: &str) -> Option<(Kind, Option<String>)> {
    let toks: Vec<(Tok, std::ops::Range<usize>)> = tokens(sql, d).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    let word = |k: usize, s: &str| toks.get(k).is_some_and(|(t, r)| *t == Tok::Word && sql[r.clone()].eq_ignore_ascii_case(s));
    if !word(0, "CREATE") {
        return None;
    }
    let at = (1..toks.len()).find(|&k| word(k, "FUNCTION") || word(k, "PROCEDURE"))?;
    let kind = if word(at, "FUNCTION") { Kind::Function } else { Kind::Procedure };
    let mut k = at + 1;
    if word(k, "IF") && word(k + 1, "NOT") && word(k + 2, "EXISTS") {
        k += 3;
    }
    let part = |k: usize| toks.get(k).filter(|(t, _)| matches!(t, Tok::Word | Tok::Quoted)).map(|(t, r)| unquote(&sql[r.clone()], *t == Tok::Quoted));
    let dot = |k: usize| toks.get(k).is_some_and(|(t, r)| *t == Tok::Punct && &sql[r.clone()] == ".");
    // A PostgreSQL name may be quoted with double quotes, which the lexer reads as a string.
    let name = if dot(k + 1) { part(k + 2) } else { part(k) };
    Some((kind, name))
}

fn unquote(tok: &str, quoted: bool) -> String {
    if !quoted {
        return tok.to_string();
    }
    if tok.len() < 2 {
        return tok.to_string();
    }
    tok[1..tok.len() - 1].replace("``", "`")
}

/// MariaDB's `OR REPLACE` put after CREATE when the text has none.
pub fn with_or_replace(d: Dialect, sql: &str) -> String {
    let toks: Vec<_> = tokens(sql, d).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment)).collect();
    let word = |k: usize, s: &str| toks.get(k).is_some_and(|(t, r)| *t == Tok::Word && sql[r.clone()].eq_ignore_ascii_case(s));
    if !word(0, "CREATE") || word(1, "OR") {
        return sql.to_string();
    }
    let end = toks[0].1.end;
    format!("{} OR REPLACE{}", &sql[..end], &sql[end..])
}

/// What saving a routine does: the statements in order, and for MySQL the definition that puts the old one back when the
/// CREATE after the DROP fails.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub steps: Vec<String>,
    pub restore: Option<String>,
    /// What the person should know before it runs.
    pub notes: Vec<String>,
}

impl Plan {
    /// The statements as the preview shows them.
    pub fn script(&self) -> String {
        self.steps.iter().map(|s| format!("{};", s.trim_end().trim_end_matches(';'))).collect::<Vec<_>>().join("\n\n")
    }
}

/// The statements for the text in the editor. `old` is the routine being edited, None for a new one. `recreate` drops the old
/// routine first, which PostgreSQL needs when the return type or an argument name changes.
pub fn plan(d: Dialect, schema: &str, old: Option<&Detail>, text: &str, recreate: bool) -> Result<Plan, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Write the routine's CREATE statement first.".into());
    }
    let Some((kind, name)) = head_of(d, text) else { return Err("The definition has to be one CREATE FUNCTION or CREATE PROCEDURE statement.".into()) };
    let mut notes = Vec::new();
    let Some(was) = old.and_then(|o| o.routine.as_ref().map(|r| (o, r))) else {
        return Ok(Plan { steps: vec![text.to_string()], restore: None, notes });
    };
    let (old, r) = was;
    if kind != r.kind {
        return Err(format!("This is a {}, so the text cannot be a {}. Create it as a new routine instead.", r.kind.noun(), kind.noun()));
    }
    if d == Dialect::Postgres {
        if recreate {
            notes.push("The old routine is dropped and created again: grants made on it are lost, and nothing that depends on it (a trigger, a view, another function) may stand in the way.".into());
            return Ok(Plan { steps: vec![drop_stmt(d, schema, r), text.to_string()], restore: None, notes });
        }
        return Ok(Plan { steps: vec![text.to_string()], restore: None, notes });
    }
    if old.header_missing.is_some() {
        return Err("Only the body could be read, so the routine cannot be saved from it.".into());
    }
    if name.as_deref().is_some_and(|n| !n.eq_ignore_ascii_case(&r.name)) {
        return Err(format!("The name is {}, not {}. A different name would make another {}; use New {} for that.", name.unwrap_or_default(), r.name, r.kind.noun(), r.kind.noun()));
    }
    if d == Dialect::MariaDb {
        notes.push("MariaDB replaces the routine in one statement.".into());
        return Ok(Plan { steps: vec![with_or_replace(d, text)], restore: None, notes });
    }
    notes.push("MySQL cannot replace a routine in place: it is dropped and created again. If the CREATE is refused, the old definition is created again. Privileges granted on this routine alone may not survive.".into());
    Ok(Plan { steps: vec![drop_stmt(d, schema, r), text.to_string()], restore: Some(old.source.clone()), notes })
}

/// A value typed into the Run dialog.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arg {
    pub value: String,
    pub null: bool,
}

/// The parameters a call takes a value for, in order.
pub fn inputs(d: &Detail) -> Vec<&Param> {
    d.params.iter().filter(|p| p.mode.takes_value()).collect()
}

/// The script that calls the routine with `args` (one for each of `inputs`). A PostgreSQL function is read with SELECT * so a
/// set-returning one gives its rows; a MySQL procedure with OUT or INOUT parameters goes through session variables that a
/// closing SELECT reads, since a bare CALL would not return them.
pub fn run_script(d: Dialect, schema: &str, detail: &Detail, args: &[Arg], bs: bool) -> Option<String> {
    let r = detail.routine.as_ref()?;
    let name = d.qualified(schema, &r.name);
    let mut next = args.iter();
    if d == Dialect::Postgres {
        let mut parts = Vec::new();
        for p in &detail.params {
            if !p.mode.takes_value() {
                // A procedure lists its OUT arguments in the call, as NULL; a function does not take them at all.
                if r.kind == Kind::Procedure {
                    parts.push("NULL".to_string());
                }
                continue;
            }
            let a = next.next().cloned().unwrap_or_default();
            let v = if a.null { format!("NULL::{}", p.ty) } else { format!("{}::{}", pg_lit(&a.value), p.ty) };
            parts.push(if p.mode == ArgMode::Variadic { format!("VARIADIC {v}") } else { v });
        }
        let call = format!("{name}({})", parts.join(", "));
        return Some(if r.kind == Kind::Procedure { format!("CALL {call}") } else { format!("SELECT * FROM {call}") });
    }
    let literal = |a: &Arg| if a.null { "NULL".to_string() } else { lit(&a.value, bs) };
    if r.kind == Kind::Function {
        let parts: Vec<String> = args.iter().map(literal).collect();
        return Some(format!("SELECT {name}({}) AS {}", parts.join(", "), d.quote_ident("result")));
    }
    let (mut sets, mut parts, mut outs) = (Vec::new(), Vec::new(), Vec::new());
    for (i, p) in detail.params.iter().enumerate() {
        let var = format!("@quarry_p{}", i + 1);
        match p.mode {
            ArgMode::Out => {
                outs.push(format!("{var} AS {}", d.quote_ident(if p.name.is_empty() { &var[1..] } else { &p.name })));
                parts.push(var);
            }
            ArgMode::InOut => {
                sets.push(format!("SET {var} = {};", literal(&next.next().cloned().unwrap_or_default())));
                outs.push(format!("{var} AS {}", d.quote_ident(if p.name.is_empty() { &var[1..] } else { &p.name })));
                parts.push(var);
            }
            _ => parts.push(literal(&next.next().cloned().unwrap_or_default())),
        }
    }
    let mut script = sets.join("\n");
    if !script.is_empty() {
        script.push('\n');
    }
    script.push_str(&format!("CALL {name}({});", parts.join(", ")));
    if !outs.is_empty() {
        script.push_str(&format!("\nSELECT {};", outs.join(", ")));
    }
    Some(script)
}

// ---------- the MySQL sequence ----------

/// Where the statements of a MySQL change run. A trait so the sequence is tested with a recording stand-in.
pub trait Steps {
    fn run(&mut self, sql: &str) -> impl std::future::Future<Output = Res<()>> + Send;
}

/// How a MySQL change ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done,
    /// A step failed. `put_back` says the old routine was created again, or why it was not.
    Failed { why: String, put_back: Result<(), String> },
}

/// Runs the plan in order. When a statement after the DROP fails, the old definition is run to put the routine back; a DROP that
/// fails changes nothing, so nothing is put back.
pub async fn run_steps(x: &mut impl Steps, plan: &Plan) -> Outcome {
    let mut dropped = false;
    for (i, s) in plan.steps.iter().enumerate() {
        if let Err(why) = x.run(s).await {
            if !dropped || plan.restore.is_none() {
                return Outcome::Failed { why, put_back: Ok(()) };
            }
            let back = x.run(plan.restore.as_deref().unwrap_or_default()).await;
            return Outcome::Failed { why, put_back: back };
        }
        dropped |= i == 0 && plan.restore.is_some();
    }
    Outcome::Done
}

/// The words of a failed change for the window. A routine that could not be put back carries its old text, so the person
/// still has it.
pub fn failure_text(plan: &Plan, why: &str, put_back: &Result<(), String>) -> String {
    match (plan.restore.as_ref(), put_back) {
        (None, _) => why.to_string(),
        (Some(_), Ok(())) if plan.steps.len() > 1 => format!("{why} The old definition was created again, so nothing changed."),
        (Some(_), Ok(())) => why.to_string(),
        (Some(old), Err(back)) => format!("{why} The routine is gone and creating the old one again failed too ({back}). Its old definition, to run by hand:\n\n{old}"),
    }
}

// ---------- the worker ----------

/// What a save answers: the routine as it is now, or why not. `recreate` says PostgreSQL refused to replace it in place and a drop
/// and create would do it.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub why: String,
    pub recreate: bool,
}

impl Failure {
    pub fn plain(why: impl Into<String>) -> Failure {
        Failure { why: why.into(), recreate: false }
    }
}

pub enum Answer {
    List { database: String, schema: String, res: Res<Vec<Routine>> },
    Detail { res: Res<Box<Detail>> },
    /// A save or a drop. The routine is what the change left, None after a drop.
    Changed { res: Result<Option<Routine>, Failure> },
}

impl Worker {
    /// The routines of `schema`; `database` says which sidebar entry asked.
    pub fn sql_routines(&self, conn: u64, db: Db, database: String, schema: String) {
        self.spawn_sql(conn, &schema.clone(), async move {
            let rows = db.meta(&schema, list_sql(db.dialect), [schema.as_str()]).await;
            let res = rows.map(|rows| rows.iter().filter_map(|r| routine_of(&|c| text(r, c))).collect());
            Msg::SqlRoutine { conn, req: 0, ans: Answer::List { database, schema, res } }
        });
    }

    /// One routine's definition and facts.
    pub fn sql_routine_detail(&self, conn: u64, req: u64, db: Db, schema: String, r: Routine) {
        self.spawn_sql(conn, &schema.clone(), async move {
            let res = if db.dialect == Dialect::Postgres { super::pg::routines::detail(&db, &schema, &r).await } else { my_read(&db, &schema, &r).await };
            Msg::SqlRoutine { conn, req, ans: Answer::Detail { res: res.map(Box::new) } }
        });
    }

    /// Saves the text of a routine, or creates it when `old` is None. Needs an unlocked connection.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_routine_save(&self, conn: u64, req: u64, db: Db, schema: String, old: Option<Detail>, text: String, recreate: bool, read_only: bool) {
        self.spawn_sql(conn, &schema.clone(), async move {
            let res = if db.dialect == Dialect::Postgres { super::pg::routines::save(&db, &schema, old.as_ref(), &text, recreate, read_only).await } else { my_save(&db, &schema, old.as_ref(), &text, read_only).await };
            Msg::SqlRoutine { conn, req, ans: Answer::Changed { res } }
        });
    }

    /// Drops a routine.
    pub fn sql_routine_drop(&self, conn: u64, req: u64, db: Db, schema: String, r: Routine, read_only: bool) {
        self.spawn_sql(conn, &schema.clone(), async move {
            let res = if db.dialect == Dialect::Postgres { super::pg::routines::remove(&db, &schema, &r, read_only).await } else { my_drop(&db, &schema, &r, read_only).await };
            Msg::SqlRoutine { conn, req, ans: Answer::Changed { res: res.map(|_| None).map_err(Failure::plain) } }
        });
    }
}

async fn my_read(db: &Db, schema: &str, r: &Routine) -> Res<Detail> {
    let args = [schema, r.name.as_str(), r.kind.word()];
    let rows = db.meta(schema, MY_DETAIL, args).await?;
    let row = rows.first().ok_or_else(|| format!("{} is gone.", r.name))?;
    let params: Vec<Param> = db.meta(schema, MY_PARAMS, args).await?.iter().filter_map(|p| param_of(&|c| text(p, c))).collect();
    // SHOW cannot bind its name and is refused by some servers as a prepared statement, so it goes over the text protocol.
    let show = format!("SHOW CREATE {} {}", r.kind.word(), db.dialect.qualified(schema, &r.name));
    let mut c = db.detached().await?;
    let t = std::time::Instant::now();
    let shown = c.fetch_all(&show).await;
    db.note(schema, &show, t, &shown);
    let _ = c.close().await;
    // A refused SHOW CREATE is not an error of the read: the body from information_schema still shows.
    let create = shown.ok().and_then(|rows| rows.first().and_then(|row| text_at(row, 2)));
    my_detail(&|c| text(row, c), params, create).ok_or_else(|| "The routine could not be read.".to_string())
}

/// The session of a MySQL change: a writable connection in the routine's database.
async fn my_conn(db: &Db, schema: &str, read_only: bool) -> Res<sqlx::MySqlConnection> {
    super::write::writer_conn(db, schema, read_only).await?.into_my()
}

struct Session<'a> {
    c: &'a mut sqlx::MySqlConnection,
    db: &'a Db,
    schema: &'a str,
}

impl Steps for Session<'_> {
    async fn run(&mut self, sql: &str) -> Res<()> {
        // Prepared, so the server parses exactly one statement whatever the text holds after it.
        let t = std::time::Instant::now();
        let res = sqlx::query(sql).persistent(false).execute(&mut *self.c).await;
        self.db.note(self.schema, sql, t, &res);
        res.map(|_| ()).map_err(se)
    }
}

async fn my_save(db: &Db, schema: &str, old: Option<&Detail>, body: &str, read_only: bool) -> Result<Option<Routine>, Failure> {
    let plan = plan(db.dialect, schema, old, body, false).map_err(Failure::plain)?;
    let (kind, name) = head_of(db.dialect, body.trim()).and_then(|(k, n)| Some((k, n?))).ok_or_else(|| Failure::plain("The name of the routine could not be read from the text."))?;
    let existing = db.meta(schema, "SELECT ROUTINE_NAME AS name FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? AND ROUTINE_TYPE = ? AND ROUTINE_NAME = ?", [schema, kind.word(), name.as_str()]).await.map_err(Failure::plain)?;
    // A new routine never replaces one: MariaDB's OR REPLACE typed into a template would otherwise overwrite it unasked.
    if old.is_none() && !existing.is_empty() {
        return Err(Failure::plain(format!("A {} named {name} already exists in {schema}. Open it to edit it, or pick another name.", kind.noun())));
    }
    let mut c = my_conn(db, schema, read_only).await.map_err(Failure::plain)?;
    let mut s = Session { c: &mut c, db, schema };
    let out = run_steps(&mut s, &plan).await;
    let _ = c.close().await;
    match out {
        Outcome::Done => {}
        Outcome::Failed { why, put_back } => return Err(Failure::plain(failure_text(&plan, &why, &put_back))),
    }
    let rows = db.meta(schema, list_sql(db.dialect), [schema]).await.map_err(Failure::plain)?;
    let now = rows.iter().filter_map(|r| routine_of(&|c| text(r, c))).find(|r| r.kind == kind && r.name.eq_ignore_ascii_case(&name));
    Ok(now)
}

async fn my_drop(db: &Db, schema: &str, r: &Routine, read_only: bool) -> Res<()> {
    let mut c = my_conn(db, schema, read_only).await?;
    let res = Session { c: &mut c, db, schema }.run(&drop_stmt(db.dialect, schema, r)).await;
    let _ = c.close().await;
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn row(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |c| m.get(c).cloned()
    }

    fn func(name: &str, args: &str) -> Routine {
        Routine { kind: Kind::Function, name: name.into(), args: args.into(), returns: "integer".into(), lang: "sql".into() }
    }

    #[test]
    fn rows_read_into_routines() {
        let r = routine_of(&row(&[("name", "add"), ("kind", "FUNCTION"), ("returns", "int(11)"), ("lang", "SQL")])).unwrap();
        assert_eq!((r.kind, r.name.as_str(), r.args.as_str()), (Kind::Function, "add", ""));
        let p = routine_of(&row(&[("name", "audit"), ("kind", "p"), ("args", "a integer, b text")])).unwrap();
        assert_eq!(p.kind, Kind::Procedure);
        assert!(routine_of(&row(&[("name", "x"), ("kind", "a")])).is_none(), "an aggregate is not listed");
        assert!(routine_of(&row(&[("kind", "f")])).is_none());
    }

    #[test]
    fn overloads_have_their_own_keys() {
        assert_ne!(func("f", "a integer").key(), func("f", "a text").key());
        let mut p = func("f", "a integer");
        p.kind = Kind::Procedure;
        assert_ne!(p.key(), func("f", "a integer").key());
        assert_eq!(func("f", "a integer").label(Dialect::Postgres), "f(a integer)");
        assert_eq!(func("f", "").label(Dialect::MySql), "f");
    }

    #[test]
    fn parameter_modes_read_for_both_engines() {
        let p = param_of(&row(&[("pname", "a"), ("mode", "b"), ("ty", "integer")])).unwrap();
        assert_eq!(p.mode, ArgMode::InOut);
        assert_eq!(param_of(&row(&[("mode", "IN"), ("ty", "int")])).unwrap().mode, ArgMode::In);
        assert_eq!(param_of(&row(&[("mode", ""), ("ty", "int")])).unwrap().mode, ArgMode::In);
        assert!(param_of(&row(&[("mode", "t"), ("ty", "int")])).is_none(), "a RETURNS TABLE column is not an argument");
    }

    #[test]
    fn postgres_detail_knows_what_may_run_read_only() {
        let base = [("name", "f"), ("kind", "f"), ("args", "a integer"), ("returns", "integer"), ("lang", "sql"), ("def", "CREATE OR REPLACE FUNCTION f()"), ("owner", "me")];
        let with = |v: &'static str| {
            let mut pairs = base.to_vec();
            pairs.push(("volatility", v));
            pg_detail(&row(&pairs), Vec::new()).unwrap()
        };
        assert!(with("s").read_only && with("i").read_only);
        assert!(!with("v").read_only);
        assert_eq!(with("s").behavior, "STABLE");
        let mut proc = base.to_vec();
        proc[1] = ("kind", "p");
        proc.push(("volatility", "i"));
        assert!(!pg_detail(&row(&proc), Vec::new()).unwrap().read_only, "a procedure is always a write");
    }

    #[test]
    fn mysql_detail_without_show_create_falls_back_to_the_body() {
        let pairs = [("name", "f"), ("kind", "FUNCTION"), ("body", "RETURN 1"), ("deterministic", "YES"), ("security", "DEFINER")];
        let d = my_detail(&row(&pairs), Vec::new(), None).unwrap();
        assert_eq!(d.source, "RETURN 1");
        assert!(d.header_missing.is_some());
        assert_eq!(d.behavior, "DETERMINISTIC");
        let full = my_detail(&row(&pairs), Vec::new(), Some("CREATE FUNCTION f() RETURNS int RETURN 1".into())).unwrap();
        assert!(full.header_missing.is_none());
        assert!(full.source.starts_with("CREATE"));
        let empty = my_detail(&row(&pairs), Vec::new(), Some("  ".into())).unwrap();
        assert!(empty.header_missing.is_some(), "a blank statement is a refused one");
    }

    #[test]
    fn drop_statements_per_dialect() {
        assert_eq!(drop_stmt(Dialect::MySql, "shop", &func("add", "")), "DROP FUNCTION `shop`.`add`");
        assert_eq!(drop_stmt(Dialect::MariaDb, "shop", &Routine { kind: Kind::Procedure, ..func("p", "") }), "DROP PROCEDURE `shop`.`p`");
        assert_eq!(drop_stmt(Dialect::Postgres, "app", &func("add", "a integer, b integer")), "DROP FUNCTION \"app\".\"add\"(a integer, b integer)");
    }

    #[test]
    fn templates_are_one_create_statement_of_their_kind() {
        for d in [Dialect::MySql, Dialect::MariaDb, Dialect::Postgres] {
            for k in [Kind::Function, Kind::Procedure] {
                let t = template(d, k, "s");
                assert_eq!(head_of(d, &t).map(|h| h.0), Some(k), "{d:?} {k:?}");
                assert!(plan(d, "s", None, &t, false).is_ok());
            }
        }
        assert!(template(Dialect::MySql, Kind::Function, "s").contains("DETERMINISTIC"));
    }

    #[test]
    fn head_reads_kind_and_name() {
        let d = Dialect::MySql;
        assert_eq!(head_of(d, "CREATE FUNCTION `shop`.`add`(a INT) RETURNS INT RETURN a"), Some((Kind::Function, Some("add".into()))));
        assert_eq!(head_of(d, "create definer=`root`@`%` procedure p() select 1"), Some((Kind::Procedure, Some("p".into()))));
        assert_eq!(head_of(d, "CREATE FUNCTION IF NOT EXISTS f() RETURNS INT RETURN 1"), Some((Kind::Function, Some("f".into()))));
        assert_eq!(head_of(d, "/* c */ CREATE OR REPLACE FUNCTION f() RETURNS INT RETURN 1").map(|h| h.0), Some(Kind::Function));
        assert!(head_of(d, "DROP FUNCTION f").is_none());
        assert!(head_of(d, "SELECT 1").is_none());
    }

    fn detail(d: Dialect, source: &str) -> Detail {
        let r = Routine { kind: Kind::Function, name: "add".into(), args: if d == Dialect::Postgres { "a integer".into() } else { String::new() }, returns: "int".into(), lang: "sql".into() };
        Detail { routine: Some(r), source: source.into(), ..Default::default() }
    }

    #[test]
    fn mysql_replaces_by_drop_then_create_with_a_way_back() {
        let old = detail(Dialect::MySql, "CREATE FUNCTION `add`(a INT) RETURNS INT RETURN a");
        let p = plan(Dialect::MySql, "shop", Some(&old), "CREATE FUNCTION `add`(a INT) RETURNS INT RETURN a + 1", false).unwrap();
        assert_eq!(p.steps, vec!["DROP FUNCTION `shop`.`add`".to_string(), "CREATE FUNCTION `add`(a INT) RETURNS INT RETURN a + 1".to_string()]);
        assert_eq!(p.restore.as_deref(), Some(old.source.as_str()));
        assert!(p.script().contains("DROP FUNCTION `shop`.`add`;"));
    }

    #[test]
    fn mariadb_replaces_in_one_statement() {
        let old = detail(Dialect::MariaDb, "CREATE FUNCTION `add`(a INT) RETURNS INT RETURN a");
        let p = plan(Dialect::MariaDb, "shop", Some(&old), "CREATE DEFINER=`root`@`%` FUNCTION `add`(a INT) RETURNS INT RETURN a + 1", false).unwrap();
        assert_eq!(p.steps.len(), 1);
        assert!(p.steps[0].starts_with("CREATE OR REPLACE DEFINER="), "{}", p.steps[0]);
        assert!(p.restore.is_none());
        assert_eq!(with_or_replace(Dialect::MariaDb, "CREATE OR REPLACE FUNCTION f() RETURNS INT RETURN 1"), "CREATE OR REPLACE FUNCTION f() RETURNS INT RETURN 1");
    }

    #[test]
    fn a_changed_name_or_kind_is_not_an_edit() {
        let old = detail(Dialect::MySql, "CREATE FUNCTION `add`(a INT) RETURNS INT RETURN a");
        let renamed = plan(Dialect::MySql, "shop", Some(&old), "CREATE FUNCTION `sum`(a INT) RETURNS INT RETURN a", false).unwrap_err();
        assert!(renamed.contains("sum") && renamed.contains("add"), "{renamed}");
        let kind = plan(Dialect::MySql, "shop", Some(&old), "CREATE PROCEDURE `add`() SELECT 1", false).unwrap_err();
        assert!(kind.contains("procedure"), "{kind}");
        let mut blind = old.clone();
        blind.header_missing = Some("no privilege".into());
        assert!(plan(Dialect::MySql, "shop", Some(&blind), "CREATE FUNCTION `add`() RETURNS INT RETURN 1", false).is_err());
        assert!(plan(Dialect::MySql, "shop", None, "  ", false).is_err());
        assert!(plan(Dialect::MySql, "shop", None, "DROP TABLE t", false).is_err());
    }

    #[test]
    fn postgres_replaces_or_drops_and_creates() {
        let old = detail(Dialect::Postgres, "CREATE OR REPLACE FUNCTION public.add(a integer)");
        let text = "CREATE OR REPLACE FUNCTION public.add(a integer) RETURNS bigint LANGUAGE sql AS $$ SELECT a::bigint $$";
        let replace = plan(Dialect::Postgres, "public", Some(&old), text, false).unwrap();
        assert_eq!(replace.steps, vec![text.to_string()]);
        let recreate = plan(Dialect::Postgres, "public", Some(&old), text, true).unwrap();
        assert_eq!(recreate.steps[0], "DROP FUNCTION \"public\".\"add\"(a integer)");
        assert_eq!(recreate.steps[1], text);
        assert!(recreate.notes.iter().any(|n| n.contains("grants")), "the warning names what is lost");
        assert!(recreate.restore.is_none(), "the transaction rolls back, so nothing needs putting back");
    }

    #[test]
    fn calls_per_dialect() {
        let mut d = detail(Dialect::MySql, "");
        d.params = vec![Param { name: "a".into(), mode: ArgMode::In, ty: "int".into() }, Param { name: "b".into(), mode: ArgMode::In, ty: "varchar(10)".into() }];
        let args = [Arg { value: "4".into(), null: false }, Arg { value: "it's".into(), null: false }];
        assert_eq!(run_script(Dialect::MySql, "shop", &d, &args, true).unwrap(), "SELECT `shop`.`add`('4', 'it''s') AS `result`");
        let nul = [Arg { value: String::new(), null: true }, args[1].clone()];
        assert!(run_script(Dialect::MySql, "shop", &d, &nul, true).unwrap().contains("(NULL, 'it''s')"));
        assert_eq!(run_script(Dialect::MySql, "shop", &d, &args, false).unwrap(), run_script(Dialect::MySql, "shop", &d, &args, true).unwrap());

        let mut p = detail(Dialect::MySql, "");
        p.routine.as_mut().unwrap().kind = Kind::Procedure;
        p.params = vec![Param { name: "x".into(), mode: ArgMode::InOut, ty: "int".into() }, Param { name: "y".into(), mode: ArgMode::Out, ty: "int".into() }, Param { name: "z".into(), mode: ArgMode::In, ty: "int".into() }];
        let args = [Arg { value: "1".into(), null: false }, Arg { value: "3".into(), null: false }];
        let s = run_script(Dialect::MySql, "shop", &p, &args, true).unwrap();
        assert_eq!(s, "SET @quarry_p1 = '1';\nCALL `shop`.`add`(@quarry_p1, @quarry_p2, '3');\nSELECT @quarry_p1 AS `x`, @quarry_p2 AS `y`;");
        assert_eq!(inputs(&p).len(), 2);
    }

    #[test]
    fn postgres_calls_cast_each_value_to_its_type() {
        let mut f = detail(Dialect::Postgres, "");
        f.params = vec![Param { name: "a".into(), mode: ArgMode::In, ty: "integer".into() }, Param { name: "r".into(), mode: ArgMode::Out, ty: "text".into() }, Param { name: "v".into(), mode: ArgMode::Variadic, ty: "integer[]".into() }];
        let args = [Arg { value: "5".into(), null: false }, Arg { value: "{1,2}".into(), null: false }];
        assert_eq!(run_script(Dialect::Postgres, "app", &f, &args, true).unwrap(), "SELECT * FROM \"app\".\"add\"('5'::integer, VARIADIC '{1,2}'::integer[])");
        let mut p = f.clone();
        p.routine.as_mut().unwrap().kind = Kind::Procedure;
        assert_eq!(run_script(Dialect::Postgres, "app", &p, &args, true).unwrap(), "CALL \"app\".\"add\"('5'::integer, NULL, VARIADIC '{1,2}'::integer[])");
        let nul = [Arg { value: String::new(), null: true }, args[1].clone()];
        assert!(run_script(Dialect::Postgres, "app", &f, &nul, true).unwrap().contains("NULL::integer"));
    }

    /// Records the statements and fails the ones named by position.
    struct Fake {
        ran: Vec<String>,
        fail: Vec<usize>,
    }

    impl Steps for Fake {
        async fn run(&mut self, sql: &str) -> Res<()> {
            self.ran.push(sql.to_string());
            if self.fail.contains(&(self.ran.len() - 1)) { Err(format!("refused {}", self.ran.len() - 1)) } else { Ok(()) }
        }
    }

    fn two_steps() -> Plan {
        Plan { steps: vec!["DROP".into(), "CREATE new".into()], restore: Some("CREATE old".into()), notes: Vec::new() }
    }

    fn block<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    #[test]
    fn a_good_change_runs_both_steps_and_no_restore() {
        let mut x = Fake { ran: Vec::new(), fail: Vec::new() };
        assert_eq!(block(run_steps(&mut x, &two_steps())), Outcome::Done);
        assert_eq!(x.ran, ["DROP", "CREATE new"]);
    }

    #[test]
    fn a_refused_create_puts_the_old_definition_back() {
        let mut x = Fake { ran: Vec::new(), fail: vec![1] };
        let out = block(run_steps(&mut x, &two_steps()));
        assert_eq!(x.ran, ["DROP", "CREATE new", "CREATE old"]);
        let Outcome::Failed { why, put_back } = out else { panic!("expected a failure") };
        assert_eq!(why, "refused 1");
        assert!(put_back.is_ok());
        assert!(failure_text(&two_steps(), &why, &put_back).contains("created again"));
    }

    #[test]
    fn a_refused_drop_changes_nothing() {
        let mut x = Fake { ran: Vec::new(), fail: vec![0] };
        let out = block(run_steps(&mut x, &two_steps()));
        assert_eq!(x.ran, ["DROP"], "no restore after a drop that did not run");
        assert!(matches!(out, Outcome::Failed { put_back: Ok(()), .. }));
    }

    #[test]
    fn a_failed_restore_hands_back_the_old_text() {
        let mut x = Fake { ran: Vec::new(), fail: vec![1, 2] };
        let Outcome::Failed { why, put_back } = block(run_steps(&mut x, &two_steps())) else { panic!("expected a failure") };
        assert!(put_back.is_err());
        let text = failure_text(&two_steps(), &why, &put_back);
        assert!(text.contains("CREATE old") && text.contains("refused 1") && text.contains("refused 2"), "{text}");
    }

    #[test]
    fn a_single_statement_change_has_nothing_to_restore() {
        let one = Plan { steps: vec!["CREATE x".into()], restore: None, notes: Vec::new() };
        let mut x = Fake { ran: Vec::new(), fail: vec![0] };
        block(run_steps(&mut x, &one));
        assert_eq!(x.ran, ["CREATE x"]);
    }
}
