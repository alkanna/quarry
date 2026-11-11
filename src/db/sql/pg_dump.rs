//! PostgreSQL SQL dumps and script runs. A dump is written from the catalogs inside one REPEATABLE READ READ ONLY
//! transaction, so it sees one snapshot and works on a locked connection, and it is a script that replays:
//!
//! 1. schemas, extensions (as `CREATE EXTENSION IF NOT EXISTS`, never their objects), types, sequences, functions that
//!    need nothing of the tables, the tables themselves (columns only), functions that use a table's row type;
//! 2. the rows as multi-row INSERTs, then each sequence's `setval` and its owner;
//! 3. primary and unique keys, checks and exclusions, domain checks, indexes, foreign keys last;
//! 4. views and materialized views in the order of their `pg_depend` edges (each materialized view with its indexes),
//!    functions that need a view, triggers, comments.
//!
//! Everything is quoted and schema qualified, and the file pins `search_path = ''` so it does not depend on the session's
//! path. The same empty path is set for the reads, so `pg_get_viewdef`, `pg_get_indexdef` and the rest print every name
//! qualified, which is the text that resolves the same way when it is written back. Values are written as the text the
//! server prints, in a literal that reads the same under both settings of standard_conforming_strings: a plain `'...'`
//! when the text has no backslash, `E'...'` with doubled backslashes when it has. Grants, owners, row security,
//! partitioning and inheritance are not dumped.

use super::csv::Source;
use super::dump::{excerpt, note, put, stopped, DumpPlan, Ran, FILE_LOGGED, INSERT_BYTES};
use super::ddl_pg::{q, qn};
use super::exec::{Cells, Conn};
use super::import::Progress;
use super::lex::dollar_quote;
use super::pg_script::PgSplitter;
use super::value::{pg_lit, pg_lit_into};
use super::{lex, se, se_at, Db, Dialect};
use crate::db::Res;
use sqlx::{raw_sql, Executor, PgConnection};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const D: Dialect = Dialect::Postgres;

/// Sequences whose positions one query reads.
const SEQ_BATCH: usize = 100;

/// A body in dollar quotes, with a tag that is not in the body.
fn dollar(body: &str) -> String {
    let tag = dollar_quote(body, "quarry");
    format!("{tag}{body}{tag}")
}

/// One result row of a catalog query, every column read as text.
struct R(Vec<Option<String>>);

impl R {
    fn s(&self, i: usize) -> String {
        self.0.get(i).cloned().flatten().unwrap_or_default()
    }

    fn o(&self, i: usize) -> Option<String> {
        self.0.get(i).cloned().flatten()
    }

    fn b(&self, i: usize) -> bool {
        self.0.get(i).and_then(|v| v.as_deref()) == Some("true")
    }

    fn n(&self, i: usize) -> i64 {
        self.0.get(i).and_then(|v| v.as_deref()).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn c(&self, i: usize) -> char {
        self.s(i).chars().next().unwrap_or(' ')
    }
}

enum Arg<'a> {
    T(&'a str),
    Ts(&'a [String]),
    Is(&'a [i64]),
}

/// One catalog query with its values bound, never spliced in. Every column of `sql` is text. `persistent(false)` because a
/// connection pooler in transaction mode cannot keep a prepared statement.
async fn cat(c: &mut PgConnection, db: &Db, sql: &str, args: &[Arg<'_>]) -> Res<Vec<R>> {
    let t = Instant::now();
    let mut query = sqlx::query(sql).persistent(false);
    for a in args {
        query = match a {
            Arg::T(s) => query.bind(s.to_string()),
            Arg::Ts(v) => query.bind(v.to_vec()),
            Arg::Is(v) => query.bind(v.to_vec()),
        };
    }
    let res = query.fetch_all(&mut *c).await;
    // A dump reads dozens of catalogs, which would push everything else out of the query log, so only a failure is listed
    // and the dump adds one line for the rest.
    if res.is_err() {
        db.note(&db.pg_database(), sql, t, &res);
    }
    let rows = res.map_err(se)?;
    Ok(rows.iter().map(|r| R((0..sqlx::Row::len(r)).map(|i| r.cell(i).map(|b| String::from_utf8_lossy(b).into_owned())).collect())).collect())
}

/// What a dumped relation is and what to write of it.
struct Rel {
    oid: i64,
    schema: String,
    name: String,
    kind: char,
    unlogged: bool,
    populated: bool,
    opts: Vec<String>,
    readable: bool,
    structure: bool,
    content: bool,
    drop: bool,
}

impl Rel {
    fn full(&self) -> String {
        qn(&self.schema, &self.name)
    }

    fn table(&self) -> bool {
        self.kind == 'r'
    }

    fn view(&self) -> bool {
        matches!(self.kind, 'v' | 'm')
    }
}

struct SeqOpts {
    schema: String,
    name: String,
    ty: String,
    start: String,
    inc: String,
    min: String,
    max: String,
    cache: String,
    cycle: bool,
}

impl SeqOpts {
    fn options(&self) -> String {
        format!("INCREMENT BY {} MINVALUE {} MAXVALUE {} START WITH {} CACHE {}{}", self.inc, self.min, self.max, self.start, self.cache, if self.cycle { " CYCLE" } else { " NO CYCLE" })
    }
}

struct Col {
    rel: i64,
    name: String,
    ty: String,
    notnull: bool,
    /// A NOT NULL constraint (PostgreSQL 18) that cannot be written inline: it is NOT VALID, named otherwise than the
    /// default or NO INHERIT. Its name and its definition, added after the rows.
    late_nn: Option<(String, String)>,
    ident: char,
    generated: char,
    def: Option<String>,
    coll: Option<(String, String)>,
    seq: Option<SeqOpts>,
}

impl Col {
    fn clause(&self) -> String {
        let mut s = format!("{} {}", q(&self.name), self.ty);
        if let Some((n, c)) = &self.coll {
            s.push_str(&format!(" COLLATE {}", qn(n, c)));
        }
        match (self.ident, self.generated, &self.def) {
            ('a' | 'd', _, _) => {
                let kind = if self.ident == 'a' { "ALWAYS" } else { "BY DEFAULT" };
                match &self.seq {
                    Some(o) => s.push_str(&format!(" GENERATED {kind} AS IDENTITY (SEQUENCE NAME {} {})", qn(&o.schema, &o.name), o.options())),
                    None => s.push_str(&format!(" GENERATED {kind} AS IDENTITY")),
                }
            }
            (_, 's' | 'v', Some(e)) => s.push_str(&format!(" GENERATED ALWAYS AS ({e}) {}", if self.generated == 'v' { "VIRTUAL" } else { "STORED" })),
            (_, _, Some(e)) => s.push_str(&format!(" DEFAULT {e}")),
            _ => {}
        }
        if self.notnull {
            s.push_str(" NOT NULL");
        }
        s
    }

    /// A computed column takes no value in an INSERT.
    fn insertable(&self) -> bool {
        self.generated == ' '
    }
}

struct Ty {
    oid: i64,
    schema: String,
    name: String,
    /// `e` enum, `d` domain, `c` composite, `r` range.
    kind: char,
    /// The relation of a composite type, whose attributes are its members.
    rel: i64,
    /// The extension that owns the type, which is created instead of the type.
    ext: Option<(String, String)>,
    /// The types it is made of, so it is written after them.
    dep: Vec<i64>,
}

struct Seq {
    oid: i64,
    schema: String,
    name: String,
    opts: SeqOpts,
    /// The table and column that own the sequence, for OWNED BY.
    owner: Option<(i64, String)>,
    readable: bool,
    create: bool,
}

struct Func {
    schema: String,
    name: String,
    args: String,
    kind: char,
    def: Option<String>,
    phase: i64,
    oid: i64,
    /// The tables it names (a row type, a table in a SQL-standard body), which have to exist before it.
    tabs: Vec<i64>,
}

fn topo(deps: &[Vec<usize>]) -> Vec<usize> {
    let (mut done, mut out) = (vec![false; deps.len()], Vec::new());
    while out.len() < deps.len() {
        let next = (0..deps.len()).find(|&i| !done[i] && deps[i].iter().all(|&j| done[j])).or_else(|| (0..deps.len()).find(|&i| !done[i])).unwrap_or(0);
        done[next] = true;
        out.push(next);
    }
    out
}

const NOT_SYSTEM: &str = "n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg\\_%'";
const NOT_EXT: &str = "NOT EXISTS (SELECT 1 FROM pg_depend x WHERE x.classid = '{cls}'::regclass AND x.objid = {oid} AND x.deptype = 'e')";

fn not_ext(cls: &str, oid: &str) -> String {
    NOT_EXT.replace("{cls}", cls).replace("{oid}", oid)
}

/// The settings the dump reads under. The empty search_path makes every printed name qualified; the others fix how values
/// print so the text reads back to the same value.
const READ_SETTINGS: &str = "SET LOCAL statement_timeout = 0; SET LOCAL search_path = ''; SET LOCAL TimeZone = 'UTC'; SET LOCAL extra_float_digits = 3; SET LOCAL standard_conforming_strings = on; SET LOCAL bytea_output = 'hex'; SET LOCAL DateStyle = 'ISO, MDY'; SET LOCAL IntervalStyle = 'postgres'";


/// Runs the whole dump on a connection of its own.
pub async fn dump(db: &Db, plan: &DumpPlan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<u64> {
    let mut conn = db.acquire_conn().await?;
    super::run::prepare(&mut conn, db, &plan.schema, true, id).await?;
    let Conn::Pg(mut c) = conn else { return Err("Not a PostgreSQL connection".into()) };
    // One message, so behind a pooler the BEGIN and the settings land on the backend of the transaction.
    let begin = db.pg_begin("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY", READ_SETTINGS);
    if let Err(x) = c.execute(begin.as_str()).await {
        let _ = c.execute("ROLLBACK").await;
        return Err(se(x));
    }
    let r = dump_on(&mut c, db, plan, out, count, cancel).await;
    // Nothing is committed: closing the connection ends the transaction, and a filter's text never outlives it.
    let _ = c.close().await;
    match r {
        Err(_) if cancel.load(Ordering::SeqCst) => Err("Export cancelled".into()),
        r => r,
    }
}

async fn dump_on(c: &mut PgConnection, db: &Db, plan: &DumpPlan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool) -> Res<u64> {
    let whole = plan.create_db;
    let started = Instant::now();
    let info = cat(c, db, "SELECT current_database()::text, pg_encoding_to_char(encoding)::text FROM pg_database WHERE datname = current_database()", &[]).await?;
    let (database, encoding) = info.first().map(|r| (r.s(0), r.s(1))).unwrap_or_default();
    put(out, &format!("-- Quarry SQL dump\n-- Server: {}\n-- Database: {}{}\n-- Server encoding: {}\n\n", note(&db.label()), note(&database), if whole { String::new() } else { format!(", schema {}", note(&plan.schema)) }, note(&encoding)))?;
    put(out, "SET client_encoding = 'UTF8';\nSET standard_conforming_strings = on;\nSET check_function_bodies = false;\nSET TimeZone = 'UTC';\nSET search_path = '';\n\n")?;

    let (rels, mut notes) = load_rels(c, db, plan, whole).await?;
    if let Some(r) = rels.iter().find(|r| r.content && !r.readable) {
        return Err(format!("The login has no SELECT privilege on {}.{}, so its rows cannot be dumped.", r.schema, r.name));
    }
    // The first catalog read took the snapshot. Locking the tables now keeps DDL off them until the transaction ends,
    // as pg_dump does, so the definitions read next still describe the rows read later.
    let locked: Vec<String> = rels.iter().filter(|r| r.table() && r.readable).map(Rel::full).collect();
    if !locked.is_empty() {
        let sql = format!("LOCK TABLE {} IN ACCESS SHARE MODE", locked.join(", "));
        let t = Instant::now();
        let res = c.execute(sql.as_str()).await;
        db.note(&database, &sql, t, &res);
        res.map_err(se)?;
    }
    stopped(cancel)?;
    let rel_ids: Vec<i64> = rels.iter().map(|r| r.oid).collect();
    let structured: Vec<i64> = rels.iter().filter(|r| r.structure).map(|r| r.oid).collect();
    let schemas = schemas_of(c, db, whole, &rels).await?;

    let types = load_types(c, db, whole, &schemas, &structured).await?;
    let seqs = load_seqs(c, db, whole, &schemas, &rel_ids, &rels).await?;
    let mut exts: Vec<(String, String)> = Vec::new();
    if whole {
        for r in cat(c, db, "SELECT e.extname::text, n.nspname::text FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname <> 'plpgsql' ORDER BY e.extname", &[]).await? {
            exts.push((r.s(0), r.s(1)));
        }
    } else if !structured.is_empty() {
        // The extensions the tables need: for their column types (below), and for an index operator class, a default or a
        // check that calls an extension's function.
        let sql = "SELECT DISTINCT e.extname::text, n.nspname::text FROM pg_depend d \
            JOIN pg_depend m ON m.classid = d.refclassid AND m.objid = d.refobjid AND m.deptype = 'e' AND m.refclassid = 'pg_extension'::regclass \
            JOIN pg_extension e ON e.oid = m.refobjid JOIN pg_namespace n ON n.oid = e.extnamespace \
            WHERE d.deptype = 'n' AND e.extname <> 'plpgsql' AND ( \
              (d.classid = 'pg_class'::regclass AND d.objid IN (SELECT i.indexrelid FROM pg_index i WHERE i.indrelid::int8 = ANY($1))) \
              OR (d.classid = 'pg_attrdef'::regclass AND d.objid IN (SELECT a.oid FROM pg_attrdef a WHERE a.adrelid::int8 = ANY($1))) \
              OR (d.classid = 'pg_constraint'::regclass AND d.objid IN (SELECT k.oid FROM pg_constraint k WHERE k.conrelid::int8 = ANY($1))) \
              OR (d.classid = 'pg_rewrite'::regclass AND d.objid IN (SELECT w.oid FROM pg_rewrite w WHERE w.ev_class::int8 = ANY($1)))) ORDER BY 1";
        for r in cat(c, db, sql, &[Arg::Is(&structured)]).await? {
            exts.push((r.s(0), r.s(1)));
        }
    }
    for t in &types {
        if let Some(e) = &t.ext {
            if !exts.contains(e) {
                exts.push(e.clone());
            }
        }
    }

    // Schemas, then extensions: a schema has to exist before what is made in it, and the types and sequences a table uses may
    // live in a schema the tables do not.
    let mut made: Vec<String> = schemas.clone();
    let others = types.iter().filter(|t| t.ext.is_none()).map(|t| &t.schema).chain(seqs.iter().filter(|s| s.create).map(|s| &s.schema)).chain(exts.iter().map(|e| &e.1));
    for s in others {
        if !made.contains(s) {
            made.push(s.clone());
        }
    }
    for s in &made {
        put(out, &format!("CREATE SCHEMA IF NOT EXISTS {};\n", q(s)))?;
    }
    for (e, s) in &exts {
        put(out, &format!("CREATE EXTENSION IF NOT EXISTS {} WITH SCHEMA {} CASCADE;\n", q(e), q(s)))?;
    }
    if !made.is_empty() {
        put(out, "\n")?;
    }

    // DROPs first, the tables in one statement so keys between them do not block it. A function that names a table's row type
    // or a view blocks the DROP of it, and the file makes the function again, so it goes first.
    let funcs = if plan.extras { load_funcs(c, db, &schemas).await? } else { Vec::new() };
    let mut dropped = false;
    let dropping = rels.iter().any(|r| r.drop && r.structure);
    for step in 0..4 {
        match step {
            0 | 2 if dropping => {
                for f in funcs.iter().filter(|f| f.def.is_some() && f.phase == if step == 0 { 2 } else { 1 }) {
                    put(out, &format!("DROP {} IF EXISTS {}({});\n", if f.kind == 'p' { "PROCEDURE" } else { "FUNCTION" }, qn(&f.schema, &f.name), f.args))?;
                    dropped = true;
                }
            }
            1 | 3 => {
                let kinds: &[(&str, char)] = if step == 1 { &[("MATERIALIZED VIEW", 'm'), ("VIEW", 'v')] } else { &[("TABLE", 'r')] };
                for (kind, k) in kinds {
                    let list: Vec<String> = rels.iter().filter(|r| r.drop && r.structure && r.kind == *k).map(Rel::full).collect();
                    if !list.is_empty() {
                        put(out, &format!("DROP {kind} IF EXISTS {};\n", list.join(", ")))?;
                        dropped = true;
                    }
                }
            }
            _ => {}
        }
    }
    if dropped {
        put(out, "\n")?;
    }

    write_types(c, db, &types, !whole, out).await?;

    // Sequences before the tables whose defaults call them.
    for s in seqs.iter().filter(|s| s.create) {
        put(out, &format!("CREATE SEQUENCE IF NOT EXISTS {} AS {} {};\n", qn(&s.schema, &s.name), s.opts.ty, s.opts.options()))?;
    }
    if seqs.iter().any(|s| s.create) {
        put(out, "\n")?;
    }

    write_funcs(&funcs, 0, out)?;

    let cols = load_cols(c, db, &rel_ids).await?;
    // The tables and the functions that name a table go in one order. A default or generated column that calls such a
    // function needs it first, the function needs the table it names, and a column typed as another table's row type needs
    // that table first.
    let by_oid: HashMap<i64, &Rel> = rels.iter().rev().map(|r| (r.oid, r)).collect();
    let mut cols_of: HashMap<i64, Vec<&Col>> = HashMap::new();
    for x in &cols {
        cols_of.entry(x.rel).or_default().push(x);
    }
    let tables: Vec<&Rel> = rels.iter().filter(|r| r.structure && r.table()).collect();
    let late: Vec<&Func> = funcs.iter().filter(|f| f.phase == 1).collect();
    let uses = if tables.is_empty() { Vec::new() } else { cat(c, db, USES, &[Arg::Is(&structured)]).await? };
    let mut uses_of: HashMap<i64, Vec<&R>> = HashMap::new();
    for u in &uses {
        uses_of.entry(u.n(0)).or_default().push(u);
    }
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); tables.len() + late.len()];
    for (i, t) in tables.iter().enumerate() {
        for u in uses_of.get(&t.oid).into_iter().flatten() {
            let at = if u.c(1) == 'p' { late.iter().position(|f| f.oid == u.n(2)).map(|p| tables.len() + p) } else { tables.iter().position(|o| o.oid == u.n(2)) };
            deps[i].extend(at.filter(|a| *a != i));
        }
    }
    for (j, f) in late.iter().enumerate() {
        deps[tables.len() + j].extend(f.tabs.iter().filter_map(|t| tables.iter().position(|o| o.oid == *t)));
    }
    for k in topo(&deps) {
        stopped(cancel)?;
        let Some(r) = tables.get(k) else {
            write_func(late[k - tables.len()], out)?;
            continue;
        };
        let list: Vec<String> = cols_of.get(&r.oid).into_iter().flatten().map(|x| format!("    {}", x.clause())).collect();
        let with = if r.opts.is_empty() { String::new() } else { format!(" WITH ({})", r.opts.join(", ")) };
        put(out, &format!("--\n-- Table {}.{}\n--\n\nCREATE {}TABLE {} (\n{}\n){};\n\n", note(&r.schema), note(&r.name), if r.unlogged { "UNLOGGED " } else { "" }, r.full(), list.join(",\n"), with))?;
    }

    let mut rows = 0u64;
    for r in rels.iter().filter(|r| r.content && r.table()) {
        stopped(cancel)?;
        put(out, &format!("--\n-- Data for {}.{}\n--\n\n", note(&r.schema), note(&r.name)))?;
        let mine: &[&Col] = cols_of.get(&r.oid).map_or(&[], Vec::as_slice);
        rows += write_rows(c, db, plan, r, mine, out, count, rows, cancel).await?;
        put(out, "\n")?;
    }

    // A sequence is not part of the snapshot, so its position is read after the rows: it is then at or past every value in
    // them.
    let mut wrote = false;
    for s in seqs.iter().filter(|s| !s.readable) {
        notes.push(format!("-- The position of sequence {}.{} was left out: the login may not read it.", note(&s.schema), note(&s.name)));
    }
    // One query for many sequences: each is a relation of its own, so they are joined with UNION ALL, in the order of the list.
    let readable: Vec<&Seq> = seqs.iter().filter(|s| s.readable).collect();
    for group in readable.chunks(SEQ_BATCH) {
        stopped(cancel)?;
        let sql = group.iter().enumerate().map(|(i, s)| format!("SELECT {i}::text, last_value::text, is_called::text FROM {}", qn(&s.schema, &s.name))).collect::<Vec<_>>().join(" UNION ALL ");
        for v in cat(c, db, &sql, &[]).await? {
            let Some(s) = group.get(v.n(0) as usize) else { continue };
            put(out, &format!("SELECT pg_catalog.setval({}, {}, {});\n", pg_lit(&qn(&s.schema, &s.name)), v.s(1), v.s(2)))?;
            wrote = true;
        }
    }
    for s in seqs.iter().filter(|s| s.create) {
        if let Some((t, col)) = &s.owner {
            if let Some(r) = by_oid.get(t).filter(|r| r.structure) {
                put(out, &format!("ALTER SEQUENCE {} OWNED BY {}.{};\n", qn(&s.schema, &s.name), r.full(), q(col)))?;
                wrote = true;
            }
        }
    }
    if wrote {
        put(out, "\n")?;
    }

    // A NOT NULL that was left out of its table: NOT VALID ones stay as they were, since the rows may hold NULLs.
    let mut wrote = false;
    for x in cols.iter().filter(|x| x.late_nn.is_some()) {
        if let (Some((name, def)), Some(r)) = (&x.late_nn, by_oid.get(&x.rel).filter(|r| r.structure && r.table())) {
            put(out, &format!("ALTER TABLE ONLY {} ADD CONSTRAINT {} {def};\n", r.full(), q(name)))?;
            wrote = true;
        }
    }
    if wrote {
        put(out, "\n")?;
    }

    // Keys, checks and indexes after the rows, so loading them does not maintain each row by row.
    let cons = load_cons(c, db, &structured).await?;
    let idxs = load_idx(c, db, &structured).await?;
    for k in cons.iter().filter(|k| k.c(3) != 'f' && k.b(8)) {
        if let Some(r) = by_oid.get(&k.n(0)) {
            put(out, &format!("ALTER TABLE ONLY {} ADD CONSTRAINT {} {};\n", r.full(), q(&k.s(2)), k.s(4)))?;
        }
    }
    let domains: Vec<i64> = types.iter().filter(|t| t.kind == 'd' && t.ext.is_none()).map(|t| t.oid).collect();
    if !domains.is_empty() {
        for k in cat(c, db, "SELECT t.oid::text, k.conname::text, pg_get_constraintdef(k.oid) FROM pg_constraint k JOIN pg_type t ON t.oid = k.contypid WHERE k.contypid::int8 = ANY($1) AND k.contype = 'c' ORDER BY t.oid, k.conname", &[Arg::Is(&domains)]).await? {
            if let Some(t) = types.iter().find(|t| t.oid == k.n(0)) {
                let stmt = format!("ALTER DOMAIN {} ADD CONSTRAINT {} {}", qn(&t.schema, &t.name), q(&k.s(1)), k.s(2));
                // On a subset the domain may exist already, with its check.
                if !whole {
                    put(out, &format!("DO {};\n", dollar(&format!("BEGIN {stmt}; EXCEPTION WHEN duplicate_object THEN NULL; END"))))?;
                } else {
                    put(out, &format!("{stmt};\n"))?;
                }
            }
        }
    }
    for i in &idxs {
        let Some(r) = by_oid.get(&i.n(0)).filter(|r| r.table()) else { continue };
        if !i.b(4) {
            notes.push(format!("-- Index {} on {}.{} was left out: the server marks it not valid (a CREATE INDEX CONCURRENTLY that failed).", note(&i.s(2)), note(&r.schema), note(&r.name)));
            continue;
        }
        // An index that backs a key is made by the key.
        if !i.b(6) {
            put(out, &format!("{};\n", i.s(3)))?;
        }
        if i.b(5) {
            put(out, &format!("ALTER TABLE {} CLUSTER ON {};\n", r.full(), q(&i.s(2))))?;
        }
    }
    let dumped: HashSet<i64> = structured.iter().copied().collect();
    for k in cons.iter().filter(|k| k.c(3) == 'f' && k.b(8)) {
        let Some(r) = by_oid.get(&k.n(0)) else { continue };
        if !dumped.contains(&k.n(5)) {
            notes.push(format!("-- Foreign key {} of {}.{} was left out: the table it points at is not part of this dump.", note(&k.s(2)), note(&r.schema), note(&r.name)));
            continue;
        }
        put(out, &format!("ALTER TABLE ONLY {} ADD CONSTRAINT {} {};\n", r.full(), q(&k.s(2)), k.s(4)))?;
    }
    put(out, "\n")?;

    write_views(c, db, &rels, out, cancel).await?;
    write_funcs(&funcs, 2, out)?;

    // Triggers after the rows, so loading them fires nothing.
    let mut trig: Vec<R> = Vec::new();
    if plan.extras {
        trig = cat(c, db, "SELECT t.tgrelid::text, t.tgname::text, pg_get_triggerdef(t.oid), t.tgenabled::text, t.oid::text FROM pg_trigger t WHERE t.tgrelid::int8 = ANY($1) AND NOT t.tgisinternal ORDER BY t.tgrelid, t.tgname", &[Arg::Is(&structured)]).await?;
        for t in &trig {
            let Some(r) = by_oid.get(&t.n(0)) else { continue };
            put(out, &format!("{};\n", t.s(2)))?;
            let how = match t.c(3) {
                'D' => "DISABLE",
                'R' => "ENABLE REPLICA",
                'A' => "ENABLE ALWAYS",
                _ => continue,
            };
            put(out, &format!("ALTER {} {} {how} TRIGGER {};\n", if r.kind == 'v' { "VIEW" } else { "TABLE" }, r.full(), q(&t.s(1))))?;
        }
        if !trig.is_empty() {
            put(out, "\n")?;
        }
    }

    write_comments(c, db, &schemas, &rels, &types, &seqs, &cons, &idxs, &funcs, &trig, out).await?;
    for n in &notes {
        put(out, &format!("{n}\n"))?;
    }
    crate::db::note_here("-- Catalog reads of the dump, in one REPEATABLE READ READ ONLY transaction", started, &Ok::<(), String>(()));
    put(out, "-- Dump complete\n")?;
    Ok(rows)
}

fn write_funcs(funcs: &[Func], phase: i64, out: &mut (dyn Write + Send)) -> Res<()> {
    for f in funcs.iter().filter(|f| f.phase == phase) {
        write_func(f, out)?;
    }
    Ok(())
}

fn write_func(f: &Func, out: &mut (dyn Write + Send)) -> Res<()> {
    match &f.def {
        Some(d) => put(out, &format!("--\n-- {} {}.{}({})\n--\n\n{};\n\n", if f.kind == 'p' { "Procedure" } else { "Function" }, note(&f.schema), note(&f.name), note(&f.args), d.trim_end())),
        None => put(out, &format!("-- Aggregate {}.{}({}) was left out: Quarry does not write aggregates.\n\n", note(&f.schema), note(&f.name), note(&f.args))),
    }
}

/// What a table needs written before it besides other kinds of object: the functions its defaults and generated columns
/// call ('p'), and the tables whose row type its columns have or hold an array of ('r').
const USES: &str = "SELECT x.rel::text, x.k, x.o::text FROM ( \
    SELECT ad.adrelid AS rel, 'p'::text AS k, d.refobjid AS o FROM pg_attrdef ad JOIN pg_depend d ON d.classid = 'pg_attrdef'::regclass AND d.objid = ad.oid AND d.refclassid = 'pg_proc'::regclass WHERE ad.adrelid::int8 = ANY($1) \
    UNION ALL SELECT a.attrelid, 'r'::text, COALESCE(NULLIF(t.typrelid, 0), e.typrelid) FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid LEFT JOIN pg_type e ON e.oid = t.typelem AND t.typlen = -1 \
    WHERE a.attrelid::int8 = ANY($1) AND a.attnum > 0 AND NOT a.attisdropped AND COALESCE(NULLIF(t.typrelid, 0), e.typrelid, 0) <> 0) x";

/// The relations to write. The second value lists what was left out and why, as comment lines for the end of the file.
async fn load_rels(c: &mut PgConnection, db: &Db, plan: &DumpPlan, whole: bool) -> Res<(Vec<Rel>, Vec<String>)> {
    let head = "SELECT c.oid::text, n.nspname::text, c.relname::text, c.relkind::text, (c.relpersistence = 'u')::text, c.relispopulated::text, COALESCE(array_to_string(c.reloptions, chr(31)), ''), has_table_privilege(c.oid, 'SELECT')::text, (c.relispartition OR c.relkind IN ('p', 'f'))::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND ";
    let rows = if whole {
        let sql = format!("{head}{NOT_SYSTEM} AND {} ORDER BY n.nspname, c.relname", not_ext("pg_class", "c.oid"));
        cat(c, db, &sql, &[]).await?
    } else {
        let names: Vec<String> = plan.tables.iter().map(|t| t.name.clone()).collect();
        let sql = format!("{head}n.nspname::text = $1 AND c.relname::text = ANY($2)");
        cat(c, db, &sql, &[Arg::T(&plan.schema), Arg::Ts(&names)]).await?
    };
    if !whole {
        if let Some(t) = plan.tables.iter().find(|t| !rows.iter().any(|r| r.s(2) == t.name)) {
            return Err(format!("{}.{} no longer exists on the server.", plan.schema, t.name));
        }
    }
    let (mut out, mut notes) = (Vec::new(), Vec::new());
    for r in &rows {
        let (schema, name, kind) = (r.s(1), r.s(2), r.c(3));
        if r.b(8) {
            // Saying nothing would be a gap nobody notices when the dump is restored.
            notes.push(format!("-- {} {}.{} was left out: partitioned tables, partitions and foreign tables are not dumped.", if kind == 'f' { "Foreign table" } else { "Table" }, note(&schema), note(&name)));
            continue;
        }
        let view = matches!(kind, 'v' | 'm');
        let (structure, content, drop) = match if whole { None } else { plan.tables.iter().find(|t| t.name == name) } {
            Some(t) => (t.structure, t.content && !view, t.drop && t.structure),
            None => (true, !view, false),
        };
        out.push(Rel { oid: r.n(0), schema, name, kind, unlogged: r.b(4), populated: r.b(5), opts: r.s(6).split('\x1f').filter(|o| !o.is_empty()).map(String::from).collect(), readable: r.b(7), structure, content, drop });
    }
    if !whole {
        // The order the dialog listed them in.
        out.sort_by_key(|r| plan.tables.iter().position(|t| t.name == r.name).unwrap_or(usize::MAX));
    }
    Ok((out, notes))
}

async fn schemas_of(c: &mut PgConnection, db: &Db, whole: bool, rels: &[Rel]) -> Res<Vec<String>> {
    if whole {
        let sql = format!("SELECT n.nspname::text FROM pg_namespace n WHERE {NOT_SYSTEM} AND {} ORDER BY n.nspname", not_ext("pg_namespace", "n.oid"));
        return Ok(cat(c, db, &sql, &[]).await?.iter().map(|r| r.s(0)).collect());
    }
    Ok(rels.iter().map(|r| r.schema.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect())
}

/// The user types the dump needs: every enum, domain, composite and range type of the schemas on a whole dump, and on any
/// dump the ones the tables' columns use, followed through arrays, domains, composite members and range subtypes.
async fn load_types(c: &mut PgConnection, db: &Db, whole: bool, schemas: &[String], structured: &[i64]) -> Res<Vec<Ty>> {
    let mut want: Vec<i64> = Vec::new();
    if whole {
        let sql = "SELECT t.oid::text FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname::text = ANY($1) AND t.typtype IN ('e', 'd', 'r', 'c') AND t.typcategory <> 'A' AND (t.typrelid = 0 OR EXISTS (SELECT 1 FROM pg_class k WHERE k.oid = t.typrelid AND k.relkind = 'c'))";
        want = cat(c, db, sql, &[Arg::Ts(schemas)]).await?.iter().map(|r| r.n(0)).collect();
    }
    if !structured.is_empty() {
        let sql = "SELECT DISTINCT a.atttypid::text FROM pg_attribute a WHERE a.attrelid::int8 = ANY($1) AND a.attnum > 0 AND NOT a.attisdropped";
        want.extend(cat(c, db, sql, &[Arg::Is(structured)]).await?.iter().map(|r| r.n(0)));
    }
    let mut seen: HashSet<i64> = HashSet::new();
    let mut elem: HashMap<i64, i64> = HashMap::new();
    let mut out: Vec<Ty> = Vec::new();
    loop {
        want.retain(|t| seen.insert(*t));
        if want.is_empty() {
            break;
        }
        let sql = "SELECT t.oid::text, n.nspname::text, t.typname::text, t.typtype::text, t.typcategory::text, t.typelem::text, t.typbasetype::text, COALESCE((SELECT k.relkind::text FROM pg_class k WHERE k.oid = t.typrelid), ''), t.typrelid::text, \
                   (SELECT e.extname::text || chr(31) || en.nspname::text FROM pg_depend d JOIN pg_extension e ON e.oid = d.refobjid JOIN pg_namespace en ON en.oid = e.extnamespace WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e' LIMIT 1) \
                   FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE t.oid::int8 = ANY($1)";
        let rows = cat(c, db, sql, &[Arg::Is(&want)]).await?;
        want = Vec::new();
        let (mut comp, mut ranges) = (Vec::new(), Vec::new());
        for r in &rows {
            let (schema, name, kind) = (r.s(1), r.s(2), r.c(3));
            if r.c(4) == 'A' {
                elem.insert(r.n(0), r.n(5));
                want.push(r.n(5));
                continue;
            }
            // The row type of a table, a view or a system type is not something to create.
            let rowtype = r.n(8) != 0 && r.c(7) != 'c';
            if schema == "information_schema" || schema.starts_with("pg_") || rowtype {
                continue;
            }
            // A type an extension owns is made by its CREATE EXTENSION, whatever its kind, so it is kept to name the extension.
            let ext = r.o(9).and_then(|s| s.split_once('\x1f').map(|(a, b)| (a.to_string(), b.to_string())));
            if ext.is_none() && !matches!(kind, 'e' | 'd' | 'r' | 'c') {
                continue;
            }
            let mut dep = Vec::new();
            match kind {
                'd' => dep.push(r.n(6)),
                'c' => comp.push(r.n(8)),
                'r' => ranges.push(r.n(0)),
                _ => {}
            }
            want.extend(dep.iter().copied());
            out.push(Ty { oid: r.n(0), schema, name, kind, rel: r.n(8), ext, dep });
        }
        if !comp.is_empty() {
            for r in cat(c, db, "SELECT a.attrelid::text, a.atttypid::text FROM pg_attribute a WHERE a.attrelid::int8 = ANY($1) AND a.attnum > 0 AND NOT a.attisdropped", &[Arg::Is(&comp)]).await? {
                if let Some(t) = out.iter_mut().find(|t| t.kind == 'c' && t.rel == r.n(0)) {
                    t.dep.push(r.n(1));
                }
                want.push(r.n(1));
            }
        }
        if !ranges.is_empty() {
            for r in cat(c, db, "SELECT r.rngtypid::text, r.rngsubtype::text FROM pg_range r WHERE r.rngtypid::int8 = ANY($1)", &[Arg::Is(&ranges)]).await? {
                if let Some(t) = out.iter_mut().find(|t| t.oid == r.n(0)) {
                    t.dep.push(r.n(1));
                }
                want.push(r.n(1));
            }
        }
    }
    // A dependency on an array type is one on its element.
    for t in &mut out {
        for d in &mut t.dep {
            while let Some(e) = elem.get(d) {
                *d = *e;
            }
        }
    }
    Ok(out)
}

/// The columns of a composite type or a table, in the clause form both use: the name, the type and the collation when it
/// is not the type's own.
const ATTRS: &str = "SELECT a.attrelid::text, a.attnum::text, a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull::text, a.attidentity::text, a.attgenerated::text, pg_get_expr(d.adbin, d.adrelid), cn.nspname::text, co.collname::text, \
     sn.nspname::text, sc.relname::text, format_type(sq.seqtypid, NULL), sq.seqstart::text, sq.seqincrement::text, sq.seqmin::text, sq.seqmax::text, sq.seqcache::text, sq.seqcycle::text, \
     CASE WHEN nk.oid IS NULL THEN NULL WHEN nk.convalidated AND NOT nk.connoinherit AND nk.conname::text = rc.relname::text || '_' || a.attname::text || '_not_null' THEN 'plain' ELSE 'named' END, nk.conname::text, pg_get_constraintdef(nk.oid) \
     FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid JOIN pg_class rc ON rc.oid = a.attrelid \
     LEFT JOIN pg_constraint nk ON nk.conrelid = a.attrelid AND nk.contype = 'n' AND nk.conkey[1] = a.attnum \
     LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
     LEFT JOIN pg_collation co ON co.oid = a.attcollation AND a.attcollation <> t.typcollation LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace \
     LEFT JOIN pg_depend dp ON a.attidentity <> '' AND dp.classid = 'pg_class'::regclass AND dp.refclassid = 'pg_class'::regclass AND dp.refobjid = a.attrelid AND dp.refobjsubid = a.attnum AND dp.deptype = 'i' \
     LEFT JOIN pg_class sc ON sc.oid = dp.objid LEFT JOIN pg_namespace sn ON sn.oid = sc.relnamespace LEFT JOIN pg_sequence sq ON sq.seqrelid = sc.oid \
     WHERE a.attrelid::int8 = ANY($1) AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attrelid, a.attnum";

async fn load_cols(c: &mut PgConnection, db: &Db, rels: &[i64]) -> Res<Vec<Col>> {
    let mut out = Vec::new();
    for r in cat(c, db, ATTRS, &[Arg::Is(rels)]).await? {
        let seq = r.o(10).zip(r.o(11)).map(|(schema, name)| SeqOpts { schema, name, ty: r.s(12), start: r.s(13), inc: r.s(14), min: r.s(15), max: r.s(16), cache: r.s(17), cycle: r.b(18) });
        // On 18 a NOT NULL is a constraint with a name. attnotnull is set even by a NOT VALID one, whose NULL rows would fail
        // the CREATE TABLE they are loaded after, and a name other than the default would be lost.
        let late_nn = if r.b(4) && r.s(19) == "named" { Some((r.s(20), r.s(21))) } else { None };
        out.push(Col { rel: r.n(0), name: r.s(2), ty: r.s(3), notnull: r.b(4) && late_nn.is_none(), late_nn, ident: r.c(5), generated: r.c(6), def: r.o(7), coll: r.o(8).zip(r.o(9)), seq });
    }
    Ok(out)
}

async fn load_seqs(c: &mut PgConnection, db: &Db, whole: bool, schemas: &[String], rel_ids: &[i64], rels: &[Rel]) -> Res<Vec<Seq>> {
    let head = "SELECT s.oid::text, n.nspname::text, s.relname::text, format_type(q.seqtypid, NULL), q.seqstart::text, q.seqincrement::text, q.seqmin::text, q.seqmax::text, q.seqcache::text, q.seqcycle::text, \
        (SELECT d.refobjid::text || chr(31) || a.attname::text FROM pg_depend d JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid WHERE d.classid = 'pg_class'::regclass AND d.objid = s.oid AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i') LIMIT 1), \
        (EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = s.oid AND d.deptype = 'i'))::text, has_sequence_privilege(s.oid, 'SELECT')::text, \
        (SELECT string_agg(u::text, ',') FROM (SELECT d.refobjid AS u FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = s.oid AND d.refclassid = 'pg_class'::regclass \
           UNION SELECT ad.adrelid FROM pg_depend d JOIN pg_attrdef ad ON ad.oid = d.objid WHERE d.classid = 'pg_attrdef'::regclass AND d.refclassid = 'pg_class'::regclass AND d.refobjid = s.oid) z) \
        FROM pg_class s JOIN pg_namespace n ON n.oid = s.relnamespace JOIN pg_sequence q ON q.seqrelid = s.oid WHERE s.relkind = 'S' AND ";
    let rows = if whole {
        let sql = format!("{head}n.nspname::text = ANY($1) AND {} ORDER BY n.nspname, s.relname", not_ext("pg_class", "s.oid"));
        cat(c, db, &sql, &[Arg::Ts(schemas)]).await?
    } else {
        // The ones the tables own or call in a default, not the sequences of the schema in general.
        let sql = format!(
            "{head}(EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = s.oid AND d.refclassid = 'pg_class'::regclass AND d.refobjid::int8 = ANY($1) AND d.deptype IN ('a', 'i')) \
             OR EXISTS (SELECT 1 FROM pg_depend d JOIN pg_attrdef ad ON ad.oid = d.objid WHERE d.classid = 'pg_attrdef'::regclass AND d.refclassid = 'pg_class'::regclass AND d.refobjid = s.oid AND ad.adrelid::int8 = ANY($1))) ORDER BY n.nspname, s.relname"
        );
        cat(c, db, &sql, &[Arg::Is(rel_ids)]).await?
    };
    let mut out = Vec::new();
    for r in &rows {
        let opts = SeqOpts { schema: r.s(1), name: r.s(2), ty: r.s(3), start: r.s(4), inc: r.s(5), min: r.s(6), max: r.s(7), cache: r.s(8), cycle: r.b(9) };
        let owner = r.o(10).and_then(|s| s.split_once('\x1f').and_then(|(t, col)| Some((t.parse::<i64>().ok()?, col.to_string()))));
        let identity = r.b(11);
        let users: Vec<i64> = r.s(13).split(',').filter_map(|t| t.parse().ok()).collect();
        // An identity column makes its own sequence. A sequence a table with structure uses is written; one that only serves
        // a table whose rows alone are dumped is already on the target.
        let create = !identity && (whole || users.iter().any(|u| rels.iter().any(|x| x.oid == *u && x.structure)));
        out.push(Seq { oid: r.n(0), schema: opts.schema.clone(), name: opts.name.clone(), opts, owner, readable: r.b(12), create });
    }
    Ok(out)
}

async fn load_funcs(c: &mut PgConnection, db: &Db, schemas: &[String]) -> Res<Vec<Func>> {
    // What a function needs decides where it goes: 1 when it names a table's row type or a table, 2 when it names a view.
    let sql = format!(
        "SELECT p.oid::text, n.nspname::text, p.proname::text, p.prokind::text, pg_get_function_identity_arguments(p.oid), CASE WHEN p.prokind IN ('f', 'p', 'w') THEN pg_get_functiondef(p.oid) END, \
        (SELECT COALESCE(max(CASE WHEN z.k IN ('v', 'm') THEN 2 ELSE 1 END), 0)::text FROM ( \
           SELECT rc.relkind::text AS k, rc.oid AS o FROM pg_depend d JOIN pg_type ty ON d.refclassid = 'pg_type'::regclass AND ty.oid = d.refobjid JOIN pg_class rc ON rc.oid = ty.typrelid WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid \
           UNION ALL SELECT rc.relkind::text, rc.oid FROM pg_depend d JOIN pg_class rc ON d.refclassid = 'pg_class'::regclass AND rc.oid = d.refobjid WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid) z WHERE z.k IN ('r', 'p', 'v', 'm', 'f')), \
        (SELECT string_agg(DISTINCT z.o::text, ',') FROM ( \
           SELECT rc.relkind::text AS k, rc.oid AS o FROM pg_depend d JOIN pg_type ty ON d.refclassid = 'pg_type'::regclass AND ty.oid = d.refobjid JOIN pg_class rc ON rc.oid = ty.typrelid WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid \
           UNION ALL SELECT rc.relkind::text, rc.oid FROM pg_depend d JOIN pg_class rc ON d.refclassid = 'pg_class'::regclass AND rc.oid = d.refobjid WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid) z WHERE z.k IN ('r', 'p', 'f')) \
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname::text = ANY($1) AND {} \
        AND NOT EXISTS (SELECT 1 FROM pg_depend i WHERE i.classid = 'pg_proc'::regclass AND i.objid = p.oid AND i.deptype = 'i') ORDER BY n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)",
        not_ext("pg_proc", "p.oid")
    );
    Ok(cat(c, db, &sql, &[Arg::Ts(schemas)]).await?.iter().map(|r| Func { oid: r.n(0), schema: r.s(1), name: r.s(2), kind: r.c(3), args: r.s(4), def: r.o(5), phase: r.n(6), tabs: r.s(7).split(',').filter_map(|t| t.parse().ok()).collect() }).collect())
}

async fn load_cons(c: &mut PgConnection, db: &Db, rels: &[i64]) -> Res<Vec<R>> {
    cat(
        c,
        db,
        "SELECT k.conrelid::text, k.oid::text, k.conname::text, k.contype::text, pg_get_constraintdef(k.oid), k.confrelid::text, k.conindid::text, k.convalidated::text, k.conislocal::text FROM pg_constraint k WHERE k.conrelid::int8 = ANY($1) AND k.contype IN ('p', 'u', 'c', 'x', 'f') \
         ORDER BY k.conrelid, CASE k.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'x' THEN 2 WHEN 'c' THEN 3 ELSE 4 END, k.conname",
        &[Arg::Is(rels)],
    )
    .await
}

async fn load_idx(c: &mut PgConnection, db: &Db, rels: &[i64]) -> Res<Vec<R>> {
    cat(
        c,
        db,
        "SELECT i.indrelid::text, i.indexrelid::text, ic.relname::text, pg_get_indexdef(i.indexrelid), i.indisvalid::text, i.indisclustered::text, \
         (EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conindid = i.indexrelid AND k.contype IN ('p', 'u', 'x') AND k.conrelid = i.indrelid))::text \
         FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid WHERE i.indrelid::int8 = ANY($1) ORDER BY i.indrelid, ic.relname",
        &[Arg::Is(rels)],
    )
    .await
}

/// The name the server gives the multirange type of a range type: "range" in the name becomes "multirange", and a name without it
/// gets a suffix.
fn default_multirange(range: &str) -> String {
    match range.find("range") {
        Some(i) => format!("{}multirange{}", &range[..i], &range[i + 5..]),
        None => format!("{range}_multirange"),
    }
}

/// Enums, domains, composite and range types, each after the types it is made of. On a table subset the target may have
/// some of them already, so each is made inside a block that ignores "already exists".
async fn write_types(c: &mut PgConnection, db: &Db, types: &[Ty], guard: bool, out: &mut (dyn Write + Send)) -> Res<()> {
    let mine: Vec<&Ty> = types.iter().filter(|t| t.ext.is_none()).collect();
    if mine.is_empty() {
        return Ok(());
    }
    let ids = |k: char| -> Vec<i64> { mine.iter().filter(|t| t.kind == k).map(|t| t.oid).collect() };
    let (enums, doms, comps, ranges) = (ids('e'), ids('d'), ids('c'), ids('r'));
    let mut labels: HashMap<i64, Vec<String>> = HashMap::new();
    if !enums.is_empty() {
        for r in cat(c, db, "SELECT enumtypid::text, enumlabel::text FROM pg_enum WHERE enumtypid::int8 = ANY($1) ORDER BY enumtypid, enumsortorder", &[Arg::Is(&enums)]).await? {
            labels.entry(r.n(0)).or_default().push(pg_lit(&r.s(1)));
        }
    }
    let mut dom: HashMap<i64, String> = HashMap::new();
    if !doms.is_empty() {
        for r in cat(
            c,
            db,
            "SELECT t.oid::text, format_type(t.typbasetype, t.typtypmod), t.typnotnull::text, pg_get_expr(t.typdefaultbin, 0), cn.nspname::text, co.collname::text FROM pg_type t \
             LEFT JOIN pg_collation co ON co.oid = t.typcollation AND t.typcollation <> (SELECT b.typcollation FROM pg_type b WHERE b.oid = t.typbasetype) LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace WHERE t.oid::int8 = ANY($1)",
            &[Arg::Is(&doms)],
        )
        .await?
        {
            let mut s = r.s(1);
            if let (Some(n), Some(c)) = (r.o(4), r.o(5)) {
                s.push_str(&format!(" COLLATE {}", qn(&n, &c)));
            }
            if let Some(d) = r.o(3) {
                s.push_str(&format!(" DEFAULT {d}"));
            }
            if r.b(2) {
                s.push_str(" NOT NULL");
            }
            dom.insert(r.n(0), s);
        }
    }
    let mut members: HashMap<i64, Vec<String>> = HashMap::new();
    if !comps.is_empty() {
        let rels: Vec<i64> = mine.iter().filter(|t| t.kind == 'c').map(|t| t.rel).collect();
        for r in cat(c, db, ATTRS, &[Arg::Is(&rels)]).await? {
            let mut s = format!("{} {}", q(&r.s(2)), r.s(3));
            if let (Some(n), Some(c)) = (r.o(8), r.o(9)) {
                s.push_str(&format!(" COLLATE {}", qn(&n, &c)));
            }
            members.entry(r.n(0)).or_default().push(s);
        }
    }
    let mut range: HashMap<i64, String> = HashMap::new();
    if !ranges.is_empty() {
        for r in cat(
            c,
            db,
            "SELECT r.rngtypid::text, format_type(r.rngsubtype, NULL), (r.rngsubdiff <> 0)::text, r.rngsubdiff::regproc::text, (r.rngcanonical <> 0)::text, oc.opcdefault::text, ocn.nspname::text, oc.opcname::text, cn.nspname::text, co.collname::text, \
             CASE WHEN current_setting('server_version_num')::int >= 140000 THEN (SELECT mt.typname::text FROM pg_type mt WHERE mt.oid = (to_jsonb(r) ->> 'rngmultitypid')::oid) END, rt.typname::text, rn.nspname::text \
             FROM pg_range r JOIN pg_type rt ON rt.oid = r.rngtypid JOIN pg_namespace rn ON rn.oid = rt.typnamespace JOIN pg_opclass oc ON oc.oid = r.rngsubopc JOIN pg_namespace ocn ON ocn.oid = oc.opcnamespace LEFT JOIN pg_collation co ON co.oid = r.rngcollation LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace WHERE r.rngtypid::int8 = ANY($1)",
            &[Arg::Is(&ranges)],
        )
        .await?
        {
            if r.b(4) {
                continue;
            }
            let mut s = format!("SUBTYPE = {}", r.s(1));
            if !r.b(5) {
                s.push_str(&format!(", SUBTYPE_OPCLASS = {}", qn(&r.s(6), &r.s(7))));
            }
            if let (Some(n), Some(c)) = (r.o(8), r.o(9)) {
                s.push_str(&format!(", COLLATION = {}", qn(&n, &c)));
            }
            if r.b(2) {
                s.push_str(&format!(", SUBTYPE_DIFF = {}", r.s(3)));
            }
            // From 14 on a range has a multirange type, named after it unless the CREATE TYPE said otherwise.
            if let Some(m) = r.o(10).filter(|m| *m != default_multirange(&r.s(11))) {
                s.push_str(&format!(", MULTIRANGE_TYPE_NAME = {}", qn(&r.s(12), &m)));
            }
            range.insert(r.n(0), s);
        }
    }
    let deps: Vec<Vec<usize>> = mine.iter().map(|t| t.dep.iter().filter_map(|d| mine.iter().position(|o| o.oid == *d)).collect()).collect();
    let mut wrote = false;
    for k in topo(&deps) {
        let t = mine[k];
        let name = qn(&t.schema, &t.name);
        let stmt = match t.kind {
            'e' => format!("CREATE TYPE {name} AS ENUM ({})", labels.get(&t.oid).map(|l| l.join(", ")).unwrap_or_default()),
            'd' => format!("CREATE DOMAIN {name} AS {}", dom.get(&t.oid).cloned().unwrap_or_default()),
            'c' => format!("CREATE TYPE {name} AS ({})", members.get(&t.rel).map(|m| m.join(", ")).unwrap_or_default()),
            _ => match range.get(&t.oid) {
                Some(r) => format!("CREATE TYPE {name} AS RANGE ({r})"),
                None => {
                    put(out, &format!("-- Range type {}.{} was left out: it has a canonical function, which Quarry does not write.\n", note(&t.schema), note(&t.name)))?;
                    continue;
                }
            },
        };
        if guard {
            put(out, &format!("DO {};\n", dollar(&format!("BEGIN {stmt}; EXCEPTION WHEN duplicate_object THEN NULL; END"))))?;
        } else {
            put(out, &format!("{stmt};\n"))?;
        }
        wrote = true;
    }
    if wrote {
        put(out, "\n")?;
    }
    Ok(())
}

/// Views and materialized views, each after the ones it reads from (the `pg_rewrite` to `pg_depend` edges), a
/// materialized view with its indexes.
async fn write_views(c: &mut PgConnection, db: &Db, rels: &[Rel], out: &mut (dyn Write + Send), cancel: &AtomicBool) -> Res<()> {
    let views: Vec<usize> = (0..rels.len()).filter(|&i| rels[i].structure && rels[i].view()).collect();
    if views.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = views.iter().map(|&i| rels[i].oid).collect();
    let defs: HashMap<i64, String> = cat(c, db, "SELECT c.oid::text, pg_get_viewdef(c.oid, true) FROM pg_class c WHERE c.oid::int8 = ANY($1)", &[Arg::Is(&ids)]).await?.iter().map(|r| (r.n(0), r.s(1))).collect();
    let edges = cat(c, db, "SELECT DISTINCT r.ev_class::text, d.refobjid::text FROM pg_rewrite r JOIN pg_depend d ON d.classid = 'pg_rewrite'::regclass AND d.objid = r.oid AND d.refclassid = 'pg_class'::regclass AND d.refobjid <> r.ev_class WHERE r.ev_class::int8 = ANY($1)", &[Arg::Is(&ids)]).await?;
    let deps: Vec<Vec<usize>> = views.iter().map(|&i| edges.iter().filter(|e| e.n(0) == rels[i].oid).filter_map(|e| views.iter().position(|&j| rels[j].oid == e.n(1))).collect()).collect();
    let idx = load_idx(c, db, &ids).await?;
    for k in topo(&deps) {
        stopped(cancel)?;
        let r = &rels[views[k]];
        let def = defs.get(&r.oid).map(|d| d.trim().trim_end_matches(';').trim_end().to_string()).unwrap_or_default();
        let check = r.opts.iter().find_map(|o| o.strip_prefix("check_option=")).map(|v| format!("\n WITH {} CHECK OPTION", v.to_ascii_uppercase())).unwrap_or_default();
        let opts: Vec<&str> = r.opts.iter().filter(|o| !o.starts_with("check_option=")).map(String::as_str).collect();
        let with = if opts.is_empty() { String::new() } else { format!(" WITH ({})", opts.join(", ")) };
        if r.kind == 'm' {
            put(out, &format!("--\n-- Materialized view {}.{}\n--\n\nCREATE MATERIALIZED VIEW {}{with} AS\n{def}\n WITH {}DATA;\n", note(&r.schema), note(&r.name), r.full(), if r.populated { "" } else { "NO " }))?;
            for i in idx.iter().filter(|i| i.n(0) == r.oid && i.b(4)) {
                put(out, &format!("{};\n", i.s(3)))?;
                if i.b(5) {
                    put(out, &format!("ALTER MATERIALIZED VIEW {} CLUSTER ON {};\n", r.full(), q(&i.s(2))))?;
                }
            }
            put(out, "\n")?;
        } else {
            put(out, &format!("--\n-- View {}.{}\n--\n\nCREATE VIEW {}{with} AS\n{def}{check};\n\n", note(&r.schema), note(&r.name), r.full()))?;
        }
    }
    Ok(())
}

/// The rows of one table as INSERT statements, `plan.rows_per_insert` rows each. Values are the text the server prints,
/// written as literals the server reads back into the column's type: an INSERT's VALUES list takes unknown-typed
/// literals as the target column's type, so no cast is needed.
#[allow(clippy::too_many_arguments)]
async fn write_rows(c: &mut PgConnection, db: &Db, plan: &DumpPlan, rel: &Rel, cols: &[&Col], out: &mut (dyn Write + Send), count: &AtomicU64, base: u64, cancel: &AtomicBool) -> Res<u64> {
    let _ = db;
    let names: Vec<String> = cols.iter().filter(|x| x.insertable()).map(|x| q(&x.name)).collect();
    if names.is_empty() {
        return Ok(0);
    }
    let mut sql = format!("SELECT {} FROM ONLY {}", names.join(", "), rel.full());
    if let Some(f) = plan.filter.as_ref().filter(|f| !f.trim().is_empty()) {
        // The clause is the user's text, so the whole statement must still be one statement that only reads.
        sql.push_str(&format!("\nWHERE (\n{}\n)", f.trim()));
        if lex::split_statements(&sql, D).len() != 1 {
            return Err("The filter must not contain another statement.".into());
        }
        lex::read_only_check(&sql, D)?;
    }
    // An identity column that is always generated only takes a value with this, which a restore of the rows needs.
    let over = if cols.iter().any(|x| x.ident == 'a') { " OVERRIDING SYSTEM VALUE" } else { "" };
    let head = format!("INSERT INTO {} ({}){over} VALUES\n", rel.full(), names.join(", "));
    let per = plan.rows_per_insert.max(1);
    let started = Instant::now();
    let (mut batch, mut n, mut rows) = (String::new(), 0usize, 0u64);
    let res: Res<()> = async {
        let mut s = raw_sql(&sql).fetch(&mut *c);
        while let Some(row) = futures_util::StreamExt::next(&mut s).await {
            let row = row.map_err(se)?;
            if n > 0 {
                batch.push_str(",\n");
            }
            batch.push('(');
            for i in 0..names.len() {
                if i > 0 {
                    batch.push_str(", ");
                }
                match row.cell(i) {
                    Some(b) => match std::str::from_utf8(b) {
                        Ok(t) => pg_lit_into(&mut batch, t),
                        Err(_) => pg_lit_into(&mut batch, &String::from_utf8_lossy(b)),
                    },
                    None => batch.push_str("NULL"),
                }
            }
            batch.push(')');
            n += 1;
            rows += 1;
            if n >= per || batch.len() >= INSERT_BYTES {
                flush(out, &head, &mut batch, &mut n)?;
                count.store(base + rows, Ordering::Relaxed);
                stopped(cancel)?;
            }
        }
        Ok(())
    }
    .await;
    crate::db::note_here(&sql, started, &res);
    res?;
    flush(out, &head, &mut batch, &mut n)?;
    count.store(base + rows, Ordering::Relaxed);
    Ok(rows)
}

fn flush(out: &mut (dyn Write + Send), head: &str, batch: &mut String, n: &mut usize) -> Res<()> {
    if *n == 0 {
        return Ok(());
    }
    put(out, head)?;
    put(out, batch)?;
    put(out, ";\n")?;
    batch.clear();
    *n = 0;
    Ok(())
}

/// Comments on what the dump wrote, last because they name objects of every kind.
#[allow(clippy::too_many_arguments)]
async fn write_comments(c: &mut PgConnection, db: &Db, schemas: &[String], rels: &[Rel], types: &[Ty], seqs: &[Seq], cons: &[R], idxs: &[R], funcs: &[Func], trig: &[R], out: &mut (dyn Write + Send)) -> Res<()> {
    let mut lines: Vec<String> = Vec::new();
    let on = |what: &str, desc: &str| format!("COMMENT ON {what} IS {};\n", pg_lit(desc));
    for r in cat(c, db, "SELECT n.nspname::text, d.description FROM pg_description d JOIN pg_namespace n ON n.oid = d.objoid WHERE d.classoid = 'pg_namespace'::regclass AND d.objsubid = 0 AND n.nspname::text = ANY($1) ORDER BY n.nspname", &[Arg::Ts(schemas)]).await? {
        lines.push(on(&format!("SCHEMA {}", q(&r.s(0))), &r.s(1)));
    }
    let mut ids: Vec<i64> = rels.iter().filter(|r| r.structure).map(|r| r.oid).collect();
    ids.extend(idxs.iter().map(|i| i.n(1)));
    ids.extend(seqs.iter().filter(|s| s.create).map(|s| s.oid));
    if !ids.is_empty() {
        for r in cat(
            c,
            db,
            "SELECT c.relkind::text, n.nspname::text, c.relname::text, d.objsubid::text, a.attname::text, d.description FROM pg_description d JOIN pg_class c ON c.oid = d.objoid JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_attribute a ON d.objsubid > 0 AND a.attrelid = d.objoid AND a.attnum = d.objsubid WHERE d.classoid = 'pg_class'::regclass AND d.objoid::int8 = ANY($1) ORDER BY n.nspname, c.relname, d.objsubid",
            &[Arg::Is(&ids)],
        )
        .await?
        {
            let full = qn(&r.s(1), &r.s(2));
            let what = if r.n(3) > 0 {
                match r.o(4) {
                    Some(col) => format!("COLUMN {full}.{}", q(&col)),
                    None => continue,
                }
            } else {
                let kind = match r.c(0) {
                    'v' => "VIEW",
                    'm' => "MATERIALIZED VIEW",
                    'i' => "INDEX",
                    'S' => "SEQUENCE",
                    _ => "TABLE",
                };
                format!("{kind} {full}")
            };
            lines.push(on(&what, &r.s(5)));
        }
    }
    let tids: Vec<i64> = types.iter().filter(|t| t.ext.is_none()).map(|t| t.oid).collect();
    if !tids.is_empty() {
        for r in cat(c, db, "SELECT n.nspname::text, t.typname::text, t.typtype::text, d.description FROM pg_description d JOIN pg_type t ON t.oid = d.objoid JOIN pg_namespace n ON n.oid = t.typnamespace WHERE d.classoid = 'pg_type'::regclass AND d.objoid::int8 = ANY($1) ORDER BY n.nspname, t.typname", &[Arg::Is(&tids)]).await? {
            lines.push(on(&format!("{} {}", if r.c(2) == 'd' { "DOMAIN" } else { "TYPE" }, qn(&r.s(0), &r.s(1))), &r.s(3)));
        }
    }
    let fids: Vec<i64> = funcs.iter().filter(|f| f.def.is_some()).map(|f| f.oid).collect();
    if !fids.is_empty() {
        for r in cat(
            c,
            db,
            "SELECT n.nspname::text, p.proname::text, p.prokind::text, pg_get_function_identity_arguments(p.oid), d.description FROM pg_description d JOIN pg_proc p ON p.oid = d.objoid JOIN pg_namespace n ON n.oid = p.pronamespace WHERE d.classoid = 'pg_proc'::regclass AND d.objoid::int8 = ANY($1) ORDER BY n.nspname, p.proname, 4",
            &[Arg::Is(&fids)],
        )
        .await?
        {
            lines.push(on(&format!("{} {}({})", if r.c(2) == 'p' { "PROCEDURE" } else { "FUNCTION" }, qn(&r.s(0), &r.s(1)), r.s(3)), &r.s(4)));
        }
    }
    let kids: Vec<i64> = cons.iter().filter(|k| k.b(8)).map(|k| k.n(1)).collect();
    if !kids.is_empty() {
        for r in cat(c, db, "SELECT k.conrelid::text, k.conname::text, d.description FROM pg_description d JOIN pg_constraint k ON k.oid = d.objoid WHERE d.classoid = 'pg_constraint'::regclass AND d.objoid::int8 = ANY($1) ORDER BY k.conrelid, k.conname", &[Arg::Is(&kids)]).await? {
            if let Some(t) = rels.iter().find(|x| x.oid == r.n(0)) {
                lines.push(on(&format!("CONSTRAINT {} ON {}", q(&r.s(1)), t.full()), &r.s(2)));
            }
        }
    }
    let trids: Vec<i64> = trig.iter().map(|t| t.n(4)).collect();
    if !trids.is_empty() {
        for r in cat(c, db, "SELECT t.tgrelid::text, t.tgname::text, d.description FROM pg_description d JOIN pg_trigger t ON t.oid = d.objoid WHERE d.classoid = 'pg_trigger'::regclass AND d.objoid::int8 = ANY($1) ORDER BY t.tgrelid, t.tgname", &[Arg::Is(&trids)]).await? {
            if let Some(t) = rels.iter().find(|x| x.oid == r.n(0)) {
                lines.push(on(&format!("TRIGGER {} ON {}", q(&r.s(1)), t.full()), &r.s(2)));
            }
        }
    }
    for l in &lines {
        put(out, l)?;
    }
    if !lines.is_empty() {
        put(out, "\n")?;
    }
    Ok(())
}

/// A script run: the statements of a file one by one, each on its own, on the unlocked connection's own session. A file
/// holds DDL and bulk loads, some of which cannot share a transaction (CREATE DATABASE, CREATE INDEX CONCURRENTLY, VACUUM),
/// and a failed statement must say what already ran, so it is autocommit, not one transaction: it stops at the first error
/// and names the statement and its line, and the statements before it stay applied. A file that opens its own transaction
/// keeps it, and one that never ends it is reported, since everything since its BEGIN was rolled back.
#[allow(clippy::too_many_arguments)]
pub async fn run_file(db: Db, schema: String, src: Source, read_only: bool, prog: Arc<Progress>, cancel: Arc<AtomicBool>, id: Arc<AtomicU64>) -> Res<Ran> {
    let mut conn = super::write::writer_conn(&db, &schema, read_only).await?;
    let pooled = super::run::prepare(&mut conn, &db, &schema, false, &id).await?;
    let Conn::Pg(mut c) = conn else { return Err("Not a PostgreSQL connection".into()) };
    // Behind a pooler nothing is set on the session: each statement is a transaction of its own that carries its settings.
    let mut locals = [super::pg::write_locals(), db.pg_run_locals(&schema).as_str()].into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("; ");
    if !pooled {
        // A restore runs long, so no statement timeout; a lock timeout so one statement never waits for another session for ever.
        c.execute(format!("SET statement_timeout = 0; SET lock_timeout = '{}s'", super::pg::WRITE_LOCK_SECS).as_str()).await.map_err(se)?;
    }
    let mut file = src.open().map_err(|x| format!("Could not read the file: {x}"))?;
    let mut sp = PgSplitter::new(false);
    let mut buf = vec![0u8; 256 << 10];
    let (mut n, mut eof, mut in_tx) = (0u64, false, false);
    let res: Res<Ran> = 'run: {
        loop {
            let piece = match sp.next_piece() {
                Ok(p) => p,
                Err(x) => break 'run Err(format!("{x}\n\n{}", stay(in_tx))),
            };
            match piece {
                Some(p) => {
                    if cancel.load(Ordering::SeqCst) {
                        break 'run Err("Cancelled.".into());
                    }
                    n += 1;
                    if two_phase(&p.text) {
                        break 'run Err(format!("Statement {n} (line {}) cannot run: PREPARE TRANSACTION, COMMIT PREPARED and ROLLBACK PREPARED hand the transaction to the server, so Quarry could not tell what a file leaves applied or rolled back. Run the file with psql.\n\n{}\n\n{}", p.line, excerpt(p.text.as_bytes()), stay(in_tx)));
                    }
                    if pooled {
                        // The header of a dump sets its search_path and the like. Nothing may stay on a shared backend, so
                        // these become settings of every statement's own transaction instead.
                        if let Some(local) = pooled_set(&p.text) {
                            locals.push_str("; ");
                            locals.push_str(&local);
                            prog.rows.store(n, Ordering::Relaxed);
                            prog.bytes.store(sp.offset(), Ordering::Relaxed);
                            continue;
                        }
                        let why = if lex::transaction_control(&p.text, D) {
                            Some(super::run::POOLER_TX)
                        } else if lex::needs_no_transaction(&p.text, D) {
                            Some(super::run::POOLER_NO_TX)
                        } else if lex::session_state(&p.text, D) {
                            Some(super::run::POOLER_SESSION)
                        } else {
                            None
                        };
                        if let Some(w) = why {
                            break 'run Err(format!("Statement {n} (line {}) cannot run: {w}\n\n{}\n\n{}", p.line, excerpt(p.text.as_bytes()), stay(in_tx)));
                        }
                    }
                    let t = Instant::now();
                    let outcome = if pooled { run_pooled(&mut c, &db, &p.text, &locals).await } else { run_text(&mut c, &p.text).await };
                    if n <= FILE_LOGGED || outcome.is_err() {
                        crate::db::note_here(&p.text[..p.text.floor_char_boundary(p.text.len().min(16_000))], t, &outcome);
                    }
                    if let Err(x) = outcome {
                        break 'run Err(format!("Statement {n} (line {}) failed: {}\n\n{}\n\n{}", p.line, se_at(&x, &p.text, 0), excerpt(p.text.as_bytes()), stay(in_tx)));
                    }
                    if let Some(open) = tx_effect(&p.text) {
                        in_tx = open;
                    }
                    // What the session does with the rest of the file can change under a statement that names no setting: the end
                    // of a transaction undoes its SET LOCALs, and a function can call set_config. A client_encoding other than
                    // UTF8 stops the run: the file is UTF-8, so the server would read other text than the splitter did.
                    if !pooled && (lex::changes_escaping(&p.text, D) || ends_scope(&p.text)) {
                        match reading(&mut c).await {
                            Ok((off, enc)) if enc.eq_ignore_ascii_case("utf8") => sp.set_plain_backslash(off),
                            Ok((_, enc)) => break 'run Err(format!("Statement {n} (line {}) set client_encoding to {enc}. Quarry sends a file as UTF-8, so anything after it would be read as other text than the file holds, and the run stops. Leave client_encoding at UTF8.\n\n{}\n\n{}", p.line, excerpt(p.text.as_bytes()), stay(in_tx))),
                            Err(x) => break 'run Err(format!("Statement {n} (line {}) was followed by a failed read of the session's settings: {}\n\n{}", p.line, se(x), stay(in_tx))),
                        }
                    }
                    prog.rows.store(n, Ordering::Relaxed);
                    prog.bytes.store(sp.offset(), Ordering::Relaxed);
                }
                None if eof => break,
                None => {
                    if cancel.load(Ordering::SeqCst) {
                        break 'run Err("Cancelled.".into());
                    }
                    match file.read(&mut buf) {
                        Ok(0) => {
                            eof = true;
                            sp.finish();
                        }
                        Ok(k) => sp.feed(&buf[..k]),
                        Err(x) => break 'run Err(format!("Could not read the file: {x}")),
                    }
                }
            }
        }
        prog.bytes.store(sp.offset(), Ordering::Relaxed);
        if in_tx {
            break 'run Err(format!("The file opened a transaction with BEGIN and never ended it, so what it did since then ({n} statements in all) was rolled back when the connection closed. Add a COMMIT."));
        }
        if n > FILE_LOGGED {
            crate::db::note_here(&format!("-- {n} statements run from a file, the first {FILE_LOGGED} are listed"), Instant::now(), &Ok::<(), String>(()));
        }
        Ok(Ran { statements: n })
    };
    let _ = c.close().await;
    match res {
        Err(x) if cancel.load(Ordering::SeqCst) => Err(format!("Run stopped. {}\n{}", stay(in_tx), crate::db::errors::stopped(x))),
        r => r,
    }
}

/// What a stop leaves: autocommit statements stay applied, but the ones inside a transaction the file opened were rolled
/// back with it.
fn stay(in_tx: bool) -> &'static str {
    if in_tx { "The transaction the file opened is rolled back; statements before its BEGIN stay applied." } else { "Statements before it ran and stay applied." }
}

/// A plain `SET name = value` of a setting that is safe to carry, as the `SET LOCAL` every transaction of the run starts
/// with. A client_encoding of UTF8 and standard_conforming_strings on are what the session has already, so they are
/// accepted and dropped. The value is built from tokens and may hold only words, names, strings, numbers and commas, never
/// a `;` or a parenthesis, so it cannot carry a second statement into every later transaction.
fn pooled_set(stmt: &str) -> Option<String> {
    use super::lex::Tok;
    let all: Vec<(Tok, &str)> = lex::tokens(stmt, D).into_iter().filter(|(t, _)| !matches!(t, Tok::Space)).map(|(t, r)| (t, &stmt[r])).collect();
    // The comment lines at the top of a dump come with its first statement.
    let toks = &all[all.iter().position(|t| t.0 != Tok::Comment).unwrap_or(all.len())..];
    // A comment inside the value would swallow what follows it once the value is joined into one message.
    if toks.iter().any(|t| t.0 == Tok::Comment) {
        return None;
    }
    let word = |i: usize| toks.get(i).filter(|t| t.0 == Tok::Word).map(|t| t.1.to_ascii_lowercase());
    if word(0)? != "set" {
        return None;
    }
    let at = if word(1)? == "session" { 2 } else { 1 };
    let name = word(at)?;
    let sep = toks.get(at + 1)?;
    if !(sep.1 == "=" || sep.1.eq_ignore_ascii_case("to")) {
        return None;
    }
    let value = &toks[at + 2..];
    let ok = |t: &(Tok, &str)| match t.0 {
        Tok::Word | Tok::Quoted | Tok::Str => true,
        Tok::Punct => matches!(t.1, "," | "-" | "+" | "."),
        _ => false,
    };
    if value.is_empty() || !value.iter().all(ok) {
        return None;
    }
    // The tokens borrow from `stmt`, so their addresses say where the value starts and ends, spacing and all.
    let from = value[0].1.as_ptr() as usize - stmt.as_ptr() as usize;
    let last = value[value.len() - 1].1;
    let text = &stmt[from..last.as_ptr() as usize - stmt.as_ptr() as usize + last.len()];
    let plain = |v: &str| v.trim_matches('\'').to_ascii_lowercase();
    match name.as_str() {
        "client_encoding" if value.len() == 1 && matches!(plain(value[0].1).as_str(), "utf8" | "utf-8" | "unicode") => Some("SET LOCAL client_min_messages = DEFAULT".into()),
        "standard_conforming_strings" if value.len() == 1 && matches!(plain(value[0].1).as_str(), "on" | "true" | "1") => Some("SET LOCAL client_min_messages = DEFAULT".into()),
        // A search_path that names pg_temp would put what the file creates on the backend, where other clients find it.
        "search_path" if text.to_ascii_lowercase().contains("pg_temp") => None,
        "search_path" | "check_function_bodies" | "timezone" | "datestyle" | "intervalstyle" | "extra_float_digits" | "client_min_messages" => Some(format!("SET LOCAL {name} = {text}")),
        _ => None,
    }
}

/// Whether a statement opens (true) or ends (false) the file's own transaction; None for any other.
fn tx_effect(stmt: &str) -> Option<bool> {
    let w = lex::head_words(stmt, D, 3);
    let at = |i: usize| w.get(i).map(String::as_str).unwrap_or("");
    match at(0) {
        "BEGIN" | "START" => Some(true),
        // ROLLBACK TO a savepoint and the two-phase forms leave the transaction as it is.
        "COMMIT" | "ROLLBACK" if at(1) == "PREPARED" || at(1) == "TO" || (at(1) == "AND" && at(2) == "CHAIN") => None,
        "COMMIT" | "END" | "ROLLBACK" | "ABORT" => Some(false),
        _ => None,
    }
}

/// One piece through the extended protocol, which the server refuses when it holds more than one statement. The simple
/// protocol would run all of them, and the splitter can disagree with the server about where a statement ends (a
/// standard_conforming_strings or client_encoding the file switched, a BEGIN ATOMIC it misread), so a statement the
/// progress and the pooler checks never saw would run. Unnamed, so no prepared statement stays on the connection.
pub(super) async fn run_text(c: &mut PgConnection, stmt: &str) -> Result<(), sqlx::Error> {
    sqlx::query(stmt).persistent(false).execute(&mut *c).await.map(|_| ())
}

/// One statement behind a pooler, in a transaction of its own that carries its settings, as a Custom Query run does.
async fn run_pooled(c: &mut PgConnection, db: &Db, stmt: &str, locals: &str) -> Result<(), sqlx::Error> {
    super::pg_read::begin_read_write(c, db, locals).await?;
    if let Err(x) = run_text(c, stmt).await {
        let _ = c.execute("ROLLBACK").await;
        return Err(x);
    }
    c.execute("COMMIT").await.map(|_| ())
}

/// How the session reads the text it is sent from now on: whether plain strings honour backslashes, and its client_encoding.
async fn reading(c: &mut PgConnection) -> Result<(bool, String), sqlx::Error> {
    let r = sqlx::query("SELECT current_setting('standard_conforming_strings'), current_setting('client_encoding')").persistent(false).fetch_one(&mut *c).await?;
    let text = |i: usize| r.cell(i).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
    Ok((text(0) != "on", text(1)))
}

/// Statements after which the session's settings may differ from before without a SET naming them: the end of a
/// transaction undoes its SET LOCALs, and ROLLBACK TO undoes a SET made after a savepoint.
fn ends_scope(stmt: &str) -> bool {
    matches!(lex::head_words(stmt, D, 1).first().map(String::as_str), Some("COMMIT" | "END" | "ROLLBACK" | "ABORT"))
}

/// Two-phase commit leaves a transaction the session no longer holds, which the run could not account for.
fn two_phase(stmt: &str) -> bool {
    let w = lex::head_words(stmt, D, 2);
    matches!((w.first().map(String::as_str), w.get(1).map(String::as_str)), (Some("PREPARE"), Some("TRANSACTION")) | (Some("COMMIT" | "ROLLBACK"), Some("PREPARED")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_reads_the_same_under_both_string_settings() {
        assert_eq!(pg_lit("plain"), "'plain'");
        assert_eq!(pg_lit("it's"), "'it''s'");
        // With a backslash only E'' reads it as one character under both settings of standard_conforming_strings.
        assert_eq!(pg_lit(r"a\b"), r"E'a\\b'");
        assert_eq!(pg_lit(r"it's \"), r"E'it''s \\'");
        assert_eq!(pg_lit("two\nlines"), "'two\nlines'");
    }

    #[test]
    fn a_range_type_names_its_multirange_like_the_server_does() {
        assert_eq!(default_multirange("floatrange"), "floatmultirange");
        assert_eq!(default_multirange("fl"), "fl_multirange");
        assert_eq!(default_multirange("range_of_x"), "multirange_of_x");
        assert_eq!(default_multirange("a_range_b"), "a_multirange_b");
    }

    #[test]
    fn a_dollar_tag_is_never_in_the_body() {
        assert_eq!(dollar("x"), "$quarry$x$quarry$");
        assert_eq!(dollar("a $quarry$ b"), "$quarryx$a $quarry$ b$quarryx$");
    }

    #[test]
    fn dependencies_come_first_and_a_loop_still_ends() {
        assert_eq!(topo(&[vec![1], vec![2], vec![]]), [2, 1, 0]);
        assert_eq!(topo(&[vec![], vec![0], vec![0, 1]]), [0, 1, 2]);
        assert_eq!(topo(&[vec![1], vec![0]]).len(), 2);
    }

    #[test]
    fn a_dump_header_set_becomes_a_setting_of_each_pooled_transaction() {
        assert_eq!(pooled_set("SET search_path = ''").as_deref(), Some("SET LOCAL search_path = ''"));
        assert_eq!(pooled_set("SET SESSION TimeZone TO 'UTC'").as_deref(), Some("SET LOCAL timezone = 'UTC'"));
        assert_eq!(pooled_set("set statement_timeout to 0"), None);
        assert_eq!(pooled_set("SET check_function_bodies = false").as_deref(), Some("SET LOCAL check_function_bodies = false"));
        assert!(pooled_set("SET client_encoding = 'UTF8'").is_some());
        assert!(pooled_set("SET standard_conforming_strings = on").is_some());
        assert_eq!(pooled_set("-- Quarry SQL dump\n-- Server: x\n\nSET search_path = ''").as_deref(), Some("SET LOCAL search_path = ''"));
        assert_eq!(pooled_set("SET search_path = \"my schema\", public, '$user'").as_deref(), Some("SET LOCAL search_path = \"my schema\", public, '$user'"));
        assert_eq!(pooled_set("SET extra_float_digits = -1").as_deref(), Some("SET LOCAL extra_float_digits = -1"));
        // Anything that could change how the rest is read, or that is not on the list, is left to the pooler checks.
        for s in ["SET standard_conforming_strings = off", "SET client_encoding = 'LATIN1'", "SET role = x", "SET LOCAL search_path = ''", "SELECT 1", "SET x"] {
            assert!(pooled_set(s).is_none(), "{s}");
        }
    }

    #[test]
    fn a_pooled_set_value_is_made_of_words_names_strings_numbers_and_commas_only() {
        // These once went into every later transaction of the run and broke each BEGIN.
        for s in ["SET search_path = 'a', (; DROP TABLE t; )", "SET search_path = a; DROP TABLE t", "SET search_path = a, (b)", "SET search_path = a -- x\n, b", "SET search_path = a /* x */, b", "SET search_path = pg_temp, public", "SET search_path = 'public, pg_temp'", "SET search_path = \"pg_temp\"", "SET timezone = a || b", "SET search_path ="] {
            assert!(pooled_set(s).is_none(), "{s}");
        }
    }

    #[test]
    fn two_phase_commit_and_the_end_of_a_scope_are_recognised() {
        for s in ["PREPARE TRANSACTION 'x'", "prepare  transaction 'x'", "COMMIT PREPARED 'x'", "ROLLBACK PREPARED 'x'", "/* c */ COMMIT PREPARED 'x'"] {
            assert!(two_phase(s), "{s}");
        }
        for s in ["PREPARE p AS SELECT 1", "COMMIT", "SELECT 'PREPARE TRANSACTION'", "ROLLBACK TO a"] {
            assert!(!two_phase(s), "{s}");
        }
        for s in ["COMMIT", "END", "rollback", "ABORT", "ROLLBACK TO SAVEPOINT a", "COMMIT AND CHAIN"] {
            assert!(ends_scope(s), "{s}");
        }
        for s in ["BEGIN", "SAVEPOINT a", "SELECT 1", "RELEASE a"] {
            assert!(!ends_scope(s), "{s}");
        }
    }

    #[test]
    fn the_transactions_a_file_opens_and_ends_are_followed() {
        assert_eq!(tx_effect("BEGIN"), Some(true));
        assert_eq!(tx_effect("start transaction isolation level serializable"), Some(true));
        for s in ["COMMIT", "END", "ROLLBACK", "abort", "COMMIT AND NO CHAIN"] {
            assert_eq!(tx_effect(s), Some(false), "{s}");
        }
        for s in ["ROLLBACK TO SAVEPOINT a", "COMMIT PREPARED 'x'", "SAVEPOINT a", "SELECT 1", "COMMIT AND CHAIN"] {
            assert_eq!(tx_effect(s), None, "{s}");
        }
    }
}
