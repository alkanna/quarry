//! The structure of one SQL table for the Structure, Relations, Triggers and Info views: columns,
//! indexes, foreign keys in both directions, triggers, the table status and the CREATE statement.
//! Everything is read-only here; `ddl` builds the ALTERs from it.

use super::se;
use super::exec::{Cells, SqlRow};
use super::value::Kind;
use super::worker::{pg_kind, text, text_at};
use super::{Db, Dialect};
use crate::db::{Msg, Res, Worker};
use sqlx::raw_sql;

#[derive(Clone, Debug, Default)]
pub struct Column {
    pub name: String,
    /// As the server writes it, such as "int unsigned" or "enum('a','b')".
    pub full_type: String,
    pub nullable: bool,
    /// "PRI", "UNI", "MUL" or empty.
    pub key: String,
    /// The default as MySQL reports it: no SQL quotes around strings. None is a real NULL.
    pub default: Option<String>,
    /// The default is an expression such as CURRENT_TIMESTAMP, not a literal.
    pub default_expr: bool,
    pub extra: String,
    pub collation: Option<String>,
    pub comment: String,
    /// MySQL's SRID attribute of a spatial column.
    pub srid: Option<u32>,
    /// MariaDB's CHECK written on the column itself, which MODIFY would drop. MySQL keeps every CHECK on the table.
    pub check: Option<String>,
    /// The kind the server's catalog gave the column, set on PostgreSQL where the type name alone cannot say (a domain
    /// over int, an enum). MySQL leaves it None and `Kind::from_column_type` reads `full_type`.
    pub kind: Option<Kind>,
    /// PostgreSQL's default exactly as `pg_get_expr` wrote it, with its cast, so a CREATE TABLE can repeat it.
    pub default_raw: Option<String>,
    /// PostgreSQL's type of the column without its length, precision or bit width, and with a domain replaced by its base
    /// type: what an import casts a value to, so the INSERT applies the length and refuses a value that does not fit.
    pub cast_type: Option<String>,
}

impl Column {
    /// "INT" for "int unsigned", "DECIMAL" for "decimal(10,2)".
    pub fn base_type(&self) -> String {
        split_type(&self.full_type).0
    }

    /// The length or the list of values: "10,2" or "'a','b'". Empty when the type has none.
    pub fn args(&self) -> String {
        split_type(&self.full_type).1
    }

    pub fn unsigned(&self) -> bool {
        split_type(&self.full_type).2
    }
}

#[derive(Clone, Debug, Default)]
pub struct IndexCol {
    pub name: String,
    pub desc: bool,
    /// How many leading characters or bytes are indexed.
    pub prefix: Option<u64>,
    /// The expression of a functional key part, which has no column name.
    pub expr: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub cols: Vec<IndexCol>,
    pub kind: String,
    pub cardinality: Option<u64>,
    pub comment: String,
    /// INVISIBLE on MySQL, IGNORED on MariaDB: kept up to date but not used by the optimizer.
    pub invisible: bool,
    /// PostgreSQL's `pg_get_indexdef`, the CREATE INDEX statement. Empty on MySQL.
    pub def: String,
    /// PostgreSQL: a primary key, unique or exclusion constraint owns the index, so it is written as that constraint.
    pub constraint: bool,
    /// PostgreSQL: the index is the table's primary key, whose name is the constraint's, not "PRIMARY".
    pub primary: bool,
    /// PostgreSQL: the index has a part its key columns do not say (a condition, expressions, included columns, an operator
    /// class, a collation or NULLS order other than the default, storage options, a tablespace, NULLS NOT DISTINCT), so
    /// dropping it and adding it again from its columns would build a different index.
    pub complex: bool,
    /// PostgreSQL: the constraint that owns the index is DEFERRABLE, and INITIALLY DEFERRED.
    pub deferrable: bool,
    pub deferred: bool,
    /// PostgreSQL: the comment on the constraint that owns the index, which is not the index's own.
    pub con_comment: String,
}

/// An object the server drops together with a PostgreSQL column, from `pg_depend`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColDep {
    pub col: String,
    /// "index", "key" (a primary key or unique constraint, which is also an index of the table), "constraint" (a check,
    /// exclusion or foreign key), "sequence", "default" (the column's own) or "other" (a trigger, statistics object,
    /// policy, generated column and the like).
    pub kind: String,
    pub name: String,
    /// The server's words for the object, for the message that names it.
    pub what: String,
    /// A constraint of this table itself, as opposed to a foreign key of another table that points at the column.
    pub own: bool,
    /// The schema and table a constraint belongs to, empty for anything else.
    pub rel_schema: String,
    pub rel_table: String,
}

/// A PostgreSQL primary key, unique, check or exclusion constraint, in the words of `pg_get_constraintdef`. Foreign
/// keys are in `Structure::foreign_keys`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Constraint {
    pub name: String,
    /// "p", "u", "c" or "x".
    pub kind: char,
    /// Such as "PRIMARY KEY (a, b)" or "CHECK ((total >= 0))".
    pub def: String,
}

/// One foreign key constraint, its columns in order. In `Structure::foreign_keys` the key belongs
/// to the table itself; in `referenced_by` it belongs to `table`, which points here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForeignKey {
    pub name: String,
    pub db: String,
    pub table: String,
    pub cols: Vec<String>,
    pub ref_db: String,
    pub ref_table: String,
    pub ref_cols: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
    /// PostgreSQL: MATCH FULL, DEFERRABLE, INITIALLY DEFERRED, and NOT VALID (the rows that were there when it was added
    /// were never checked). All false on MySQL.
    pub match_full: bool,
    pub deferrable: bool,
    pub deferred: bool,
    pub not_valid: bool,
    /// PostgreSQL: the comment on the constraint.
    pub comment: String,
    /// PostgreSQL: why this key cannot be written back (an ON DELETE column list, PERIOD, NOT ENFORCED), empty when it can.
    pub opaque: String,
    /// PostgreSQL: the referenced table's key columns and unique indexes as the dialog read them (`pg::ddl::ref_state`),
    /// which the transaction must find again after it holds the lock. Empty for a key read from the catalog.
    pub ref_seen: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trigger {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub body: String,
    pub definer: String,
    /// ACTION_ORDER: where it fires among the triggers of the same timing and event, from 1.
    pub order: u64,
    /// The sql_mode it was created under, which its body runs with.
    pub sql_mode: String,
    /// PostgreSQL: what editing the trigger and its function needs. None on MySQL.
    pub pg: Option<Box<PgTrig>>,
}

/// A PostgreSQL trigger as the catalogs hold it, read with `pg::catalog::TRIGGERS`. The trigger is the part on the table; the
/// function it runs is a separate object, often shared, and is read along because the editor writes both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PgTrig {
    pub per_row: bool,
    /// The columns of UPDATE OF, empty for any column.
    pub of_cols: Vec<String>,
    /// The WHEN condition without its parentheses.
    pub when: String,
    /// tgenabled: O (enabled), D (disabled), R (replica only) or A (always).
    pub mode: String,
    pub comment: String,
    /// The relkind of the table: r, p, v or f.
    pub rel_kind: String,
    pub func_schema: String,
    pub func: String,
    pub lang: String,
    /// The function's body, which is the text between its dollar quotes.
    pub src: String,
    /// `pg_get_functiondef`: the statement that creates the function again.
    pub func_def: String,
    /// How many other triggers, on this table or others, run the function.
    pub func_others: u64,
    pub func_owner: String,
    pub func_mine: bool,
    pub func_can: bool,
    /// Whether the role has CREATE on the function's schema, which putting a dropped function back needs.
    pub func_create: bool,
    pub func_secdef: bool,
    pub func_acl: bool,
    pub func_comment: String,
    pub func_ext: bool,
    /// `pg_get_triggerdef`: the statement that creates the trigger again.
    pub trig_def: String,
    /// What the transaction must find again under the table's lock (`pg::catalog::SEEN_TRIGGER` and `SEEN_FUNCTION`).
    pub seen_trig: String,
    pub seen_func: String,
    /// Why no change of the trigger can be written back, and why it cannot be created again (its function can still change).
    pub refuse: String,
    pub no_rewrite: String,
}

impl Trigger {
    /// The statement as the Triggers view shows it. MySQL stores only the body. PostgreSQL's body is the whole CREATE
    /// TRIGGER statement, because the trigger runs a function that lives apart from it.
    pub fn create_sql(&self, table: &str) -> String {
        if self.body.trim_start().get(..7).is_some_and(|p| p.eq_ignore_ascii_case("CREATE ")) {
            return self.body.clone();
        }
        format!("CREATE TRIGGER {} {} {} ON {}\nFOR EACH ROW\n{}", self.name, self.timing, self.event, table, self.body)
    }
}

impl ForeignKey {
    /// The WHERE text that finds the row this key points to, from the literals of the key columns in order.
    /// Written as a flat AND list of equalities so the Content bar can read it back as rules.
    pub fn row_where(&self, d: Dialect, lits: &[String]) -> String {
        self.ref_cols.iter().zip(lits).map(|(c, v)| format!("{} = {v}", d.quote_ident(c))).collect::<Vec<_>>().join(" AND ")
    }
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub engine: String,
    pub row_format: String,
    pub collation: String,
    pub rows: Option<u64>,
    pub avg_row: Option<u64>,
    pub data: Option<u64>,
    pub index: Option<u64>,
    pub free: Option<u64>,
    pub auto_increment: Option<u64>,
    pub created: String,
    pub updated: String,
    pub comment: String,
    /// PostgreSQL's `pg_total_relation_size`: the heap, its TOAST table and the indexes.
    pub total: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Structure {
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    pub referenced_by: Vec<ForeignKey>,
    pub triggers: Vec<Trigger>,
    pub status: Status,
    pub create: String,
    /// PostgreSQL primary key, unique, check and exclusion constraints. Empty on MySQL, whose keys are indexes and
    /// whose CHECKs are not read here.
    pub constraints: Vec<Constraint>,
    /// The schema of a PostgreSQL table, which also says the structure came from PostgreSQL. None on MySQL, where the
    /// database is the namespace and the statements are written for a session switched to it.
    pub pg_schema: Option<String>,
    /// PostgreSQL: what depends on each column, so dropping a column can say what goes with it. Empty on MySQL.
    pub pg_deps: Vec<ColDep>,
}

impl Structure {
    /// Whether the structure came from PostgreSQL (it is the one that carries its schema).
    pub fn is_pg(&self) -> bool {
        self.pg_schema.is_some()
    }

    /// The index that serves a foreign key: the first one that starts with the key's columns.
    pub fn index_for(&self, fk: &ForeignKey) -> Option<&Index> {
        self.indexes.iter().find(|ix| ix.cols.len() >= fk.cols.len() && ix.cols.iter().zip(&fk.cols).all(|(a, b)| &a.name == b))
    }
}

/// ("INT", "10,2", true) for "int(10,2) unsigned zerofill"-like text. The last closing parenthesis
/// ends the arguments, because enum values may hold parentheses but nothing follows them except words.
pub fn split_type(full: &str) -> (String, String, bool) {
    let (head, args, tail) = match (full.find('('), full.rfind(')')) {
        (Some(a), Some(b)) if b > a => (&full[..a], full[a + 1..b].to_string(), &full[b + 1..]),
        _ => (full, String::new(), ""),
    };
    let rest = format!("{head} {tail}").to_ascii_uppercase();
    let unsigned = rest.split_whitespace().any(|w| w == "UNSIGNED");
    let base = head.split_whitespace().next().unwrap_or_default().to_ascii_uppercase();
    (base, args, unsigned)
}

/// Makes MariaDB's COLUMN_DEFAULT read like MySQL's. MariaDB keeps the SQL quotes around string
/// defaults, writes the text NULL for a nullable column without default and names expressions such
/// as current_timestamp(). Returns the default and whether it is an expression.
pub fn normalize_default(d: Dialect, raw: Option<String>, nullable: bool, extra: &str) -> (Option<String>, bool) {
    let Some(v) = raw else { return (None, false) };
    if matches!(d, Dialect::Postgres) {
        return pg_default(&v);
    }
    if matches!(d, Dialect::MySql) {
        return (Some(v), extra.to_ascii_uppercase().contains("DEFAULT_GENERATED"));
    }
    if v == "NULL" && nullable {
        return (None, false);
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return (Some(unquote(&v[1..v.len() - 1])), false);
    }
    let literal = v == "NULL" || v.parse::<f64>().is_ok();
    (Some(v), !literal)
}

/// Reads `pg_get_expr` of a column default into what MySQL's readers give: a literal without its quotes and cast, or an
/// expression as written. `'pending'::character varying` is the literal pending, `0` and `true` are literals, `now()` and
/// `nextval('s'::regclass)` are expressions, and `NULL::text` is no default.
fn pg_default(v: &str) -> (Option<String>, bool) {
    let t = v.trim();
    let first = t.chars().next().unwrap_or(' ');
    if t.eq_ignore_ascii_case("null") || t.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("null::")) && is_cast(&t[4..]) {
        return (None, false);
    }
    if let Some((inner, escapes, rest)) = split_literal(t) && (rest.is_empty() || is_cast(rest)) {
        return (Some(if escapes { unquote(&inner) } else { inner.replace("''", "'") }), false);
    }
    if (first.is_ascii_digit() || first == '-' || first == '.') && t.parse::<f64>().is_ok() || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        return (Some(t.to_string()), false);
    }
    (Some(t.to_string()), true)
}

/// `::type` repeated, where a type is words, a schema dot, a length or a `[]`, never a quote or an operator.
fn is_cast(s: &str) -> bool {
    s.starts_with("::") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b" _.\"(),[]:".contains(&b))
}

/// The inside of a leading `'...'` or `E'...'` literal (still with doubled quotes), whether it is the escape kind, and
/// what follows it.
fn split_literal(t: &str) -> Option<(String, bool, &str)> {
    let (escapes, body) = match t.strip_prefix(['E', 'e']) {
        Some(b) if b.starts_with('\'') => (true, b),
        _ => (false, t),
    };
    let b = body.strip_prefix('\'')?;
    let bytes = b.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if escapes => i += 2,
            b'\'' if bytes.get(i + 1) == Some(&b'\'') => i += 2,
            b'\'' => return Some((b[..i].to_string(), escapes, &b[i + 1..])),
            _ => i += 1,
        }
    }
    None
}

/// The text inside a MariaDB default literal: it doubles quotes and writes backslashes as escapes.
fn unquote(inner: &str) -> String {
    let mut out = String::new();
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => out.push(match it.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some('Z') => '\x1a',
                Some(o) => o,
                None => '\\',
            }),
            '\'' if it.peek() == Some(&'\'') => {
                it.next();
                out.push('\'');
            }
            c => out.push(c),
        }
    }
    out
}

/// An unsigned column value, whichever width and signedness the server reports.
pub(super) fn num<R: Cells>(r: &R, name: &str) -> Option<u64> {
    r.uint(name)
}

fn s(r: &SqlRow, name: &str) -> String {
    text(r, name).unwrap_or_default()
}

pub(super) fn columns(d: Dialect, rows: &[SqlRow]) -> Vec<Column> {
    rows.iter()
        .map(|r| {
            let nullable = r.int("nullable").map(|n| n != 0).unwrap_or(true);
            let extra = s(r, "extra");
            let raw = text(r, "col_default");
            let (default, default_expr) = normalize_default(d, raw.clone(), nullable, &extra);
            let pg = d == Dialect::Postgres;
            Column { name: s(r, "name"), full_type: s(r, "full_type"), nullable, key: s(r, "col_key"), default, default_expr, extra, collation: text(r, "collation"), comment: s(r, "comment"), srid: None, check: None, kind: pg.then(|| pg_kind(r)), default_raw: if pg { raw } else { None }, cast_type: if pg { text(r, "cast_type") } else { None } }
        })
        .collect()
}

/// The rows come ordered by index name and then position, so one index is a run of rows.
pub(super) fn indexes(rows: &[SqlRow]) -> Vec<Index> {
    let mut out: Vec<Index> = Vec::new();
    for r in rows {
        let name = s(r, "name");
        // Only PostgreSQL has key parts that are expressions, and it gives their text in `expr` with an empty name.
        let col = IndexCol { name: s(r, "col"), desc: text(r, "direction").as_deref() == Some("D"), prefix: num(r, "sub_part"), expr: text(r, "expr") };
        if out.last().is_some_and(|i| i.name == name) {
            let last = out.last_mut().unwrap();
            last.cols.push(col);
            last.complex |= r.int("complex") == Some(1);
        } else {
            let unique = r.int("non_unique").map(|n| n == 0).unwrap_or(false);
            out.push(Index { name, unique, cols: vec![col], kind: s(r, "index_type"), cardinality: num(r, "cardinality"), comment: s(r, "comment"), invisible: false, def: s(r, "def"), constraint: r.int("backs_constraint") == Some(1), primary: r.int("is_primary") == Some(1), complex: r.int("complex") == Some(1), deferrable: r.int("defer_ok") == Some(1), deferred: r.int("defer_init") == Some(1), con_comment: s(r, "con_comment") });
        }
    }
    out
}

pub(super) fn col_deps(rows: &[SqlRow]) -> Vec<ColDep> {
    rows.iter().map(|r| ColDep { col: s(r, "col"), kind: s(r, "kind"), name: s(r, "name"), what: s(r, "what"), own: flag(r, "own"), rel_schema: s(r, "rel_schema"), rel_table: s(r, "rel_table") }).collect()
}

/// A 0 or 1 column that MySQL's key query does not have, which reads as false there.
fn flag(r: &SqlRow, name: &str) -> bool {
    r.int(name).is_some_and(|n| n != 0)
}

/// One row per key column, ordered by constraint and then position.
pub(super) fn foreign_keys(rows: &[SqlRow]) -> Vec<ForeignKey> {
    let mut out: Vec<ForeignKey> = Vec::new();
    for r in rows {
        let (name, db, table) = (s(r, "name"), s(r, "db"), s(r, "tbl"));
        if let Some(k) = out.last_mut().filter(|k| k.name == name && k.db == db && k.table == table) {
            k.cols.push(s(r, "col"));
            k.ref_cols.push(s(r, "ref_col"));
        } else {
            out.push(ForeignKey { name, db, table, cols: vec![s(r, "col")], ref_db: s(r, "ref_db"), ref_table: s(r, "ref_tbl"), ref_cols: vec![s(r, "ref_col")], on_update: s(r, "on_update"), on_delete: s(r, "on_delete"), match_full: flag(r, "match_full"), deferrable: flag(r, "defer_ok"), deferred: flag(r, "defer_init"), not_valid: flag(r, "not_valid"), comment: s(r, "comment"), opaque: s(r, "opaque"), ref_seen: Vec::new() });
        }
    }
    out
}

fn triggers(rows: &[SqlRow]) -> Vec<Trigger> {
    rows.iter().map(|r| Trigger { name: s(r, "name"), timing: s(r, "timing"), event: s(r, "event"), body: s(r, "body"), definer: s(r, "definer"), order: num(r, "ord").unwrap_or(0), sql_mode: s(r, "sql_mode"), pg: pg_trigger(r) }).collect()
}

/// The PostgreSQL columns of a trigger row, absent on MySQL's.
fn pg_trigger(r: &SqlRow) -> Option<Box<PgTrig>> {
    text(r, "fn_name")?;
    Some(Box::new(PgTrig {
        per_row: flag(r, "per_row"),
        of_cols: serde_json::from_str(&s(r, "of_cols")).unwrap_or_default(),
        when: s(r, "when_text"),
        mode: s(r, "mode"),
        comment: s(r, "comment"),
        rel_kind: s(r, "rel_kind"),
        func_schema: s(r, "fn_schema"),
        func: s(r, "fn_name"),
        lang: s(r, "fn_lang"),
        src: s(r, "fn_src"),
        func_def: s(r, "fn_def"),
        func_others: num(r, "fn_others").unwrap_or(0),
        func_owner: s(r, "fn_owner"),
        func_mine: flag(r, "fn_mine"),
        func_can: flag(r, "fn_can"),
        func_create: flag(r, "fn_create"),
        func_secdef: flag(r, "fn_secdef"),
        func_acl: flag(r, "fn_acl"),
        func_comment: s(r, "fn_comment"),
        func_ext: flag(r, "fn_ext"),
        trig_def: s(r, "trig_def"),
        seen_trig: s(r, "seen_trig"),
        seen_func: s(r, "seen_func"),
        refuse: s(r, "refuse"),
        no_rewrite: s(r, "no_rewrite"),
    }))
}

/// The triggers of a table read on a connection of the caller's, for the trigger jobs that must see what the
/// connection they write on sees.
pub(super) async fn triggers_on(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, table: &str) -> Res<Vec<Trigger>> {
    let rows = SqlRow::wrap(sqlx::query(d.triggers()).bind(schema).bind(table).fetch_all(&mut *c).await.map_err(se)?);
    Ok(triggers(&rows))
}

/// The rows of a bound metadata query that `Db::meta` read, as the Postgres and MySQL readers share them.
fn first_text(rows: &[SqlRow]) -> String {
    rows.first().and_then(|r| text_at(r, 0)).unwrap_or_default()
}

fn status(rows: &[SqlRow]) -> Status {
    let Some(r) = rows.first() else { return Status::default() };
    Status {
        engine: s(r, "engine"),
        row_format: s(r, "row_format"),
        collation: s(r, "collation"),
        rows: num(r, "row_estimate"),
        avg_row: num(r, "avg_row_bytes"),
        data: num(r, "data_bytes"),
        index: num(r, "index_bytes"),
        free: num(r, "free_bytes"),
        auto_increment: num(r, "auto_increment"),
        created: s(r, "created"),
        updated: s(r, "updated"),
        comment: s(r, "comment"),
        total: num(r, "total_bytes"),
    }
}

pub(super) fn constraints(rows: &[SqlRow]) -> Vec<Constraint> {
    rows.iter().map(|r| Constraint { name: s(r, "name"), kind: s(r, "kind").chars().next().unwrap_or('c'), def: s(r, "def") }).collect()
}

/// A LIMIT of its own overrides the session's sql_select_limit, which a server's globals can set and which would
/// show a table with only its first columns or indexes, and let an edit of that structure drop the rest.
pub(super) const ALL_ROWS: &str = " LIMIT 18446744073709551615";

/// On PostgreSQL the read runs under the table's schema as search_path, the one a structure change writes its statements
/// under, so a type or function outside it comes back qualified and means the same when it is written back.
async fn query(db: &Db, sql: &str, schema: &str, table: &str) -> Res<Vec<SqlRow>> {
    db.meta_with(schema, sql, [schema, table], true).await
}

/// What the main queries cannot ask every server version for: SRIDs, invisible indexes and functional key parts on
/// MySQL 8, ignored indexes and column CHECKs on MariaDB. An older server fails these queries, and has none of it.
async fn extras(db: &Db, schema: &str, table: &str, st: &mut Structure) -> Res<()> {
    let d = db.dialect;
    let (idx, cols) = match d {
        // The key parts, hidden state and SRIDs are already in the main queries. What a CREATE TABLE still needs is the
        // constraints that are not foreign keys.
        Dialect::Postgres => {
            if let Ok(rows) = query(db, super::pg::catalog::CONSTRAINTS, schema, table).await {
                st.constraints = constraints(&rows);
            }
            // Not optional: an undo of a dropped column trusts this list to say what went with the column.
            st.pg_deps = col_deps(&query(db, super::pg::catalog::COLUMN_DEPS, schema, table).await?);
            return Ok(());
        }
        Dialect::MySql => (
            "SELECT INDEX_NAME AS name, SEQ_IN_INDEX AS seq, IS_VISIBLE = 'NO' AS hidden, EXPRESSION AS expr FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
            "SELECT COLUMN_NAME AS name, SRS_ID AS srid, NULL AS clause FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND SRS_ID IS NOT NULL",
        ),
        Dialect::MariaDb => (
            "SELECT INDEX_NAME AS name, SEQ_IN_INDEX AS seq, IGNORED = 'YES' AS hidden, NULL AS expr FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
            // A column CHECK is named after its column.
            "SELECT CONSTRAINT_NAME AS name, NULL AS srid, CHECK_CLAUSE AS clause FROM information_schema.CHECK_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = ? AND TABLE_NAME = ? AND LEVEL = 'Column'",
        ),
    };
    let (idx, cols) = tokio::join!(query(db, idx, schema, table), query(db, cols, schema, table));
    for r in idx.unwrap_or_default() {
        let (name, seq) = (s(&r, "name"), num(&r, "seq").unwrap_or(0) as usize);
        let Some(ix) = st.indexes.iter_mut().find(|i| i.name == name) else { continue };
        ix.invisible |= r.int("hidden").is_some_and(|n| n != 0);
        if let (Some(e), Some(c)) = (text(&r, "expr"), seq.checked_sub(1).and_then(|k| ix.cols.get_mut(k))) {
            c.expr = Some(e);
        }
    }
    for r in cols.unwrap_or_default() {
        let name = s(&r, "name");
        let Some(c) = st.columns.iter_mut().find(|c| c.name == name) else { continue };
        c.srid = num(&r, "srid").and_then(|n| u32::try_from(n).ok());
        c.check = text(&r, "clause");
    }
    Ok(())
}

pub(super) async fn load(db: &Db, schema: &str, table: &str) -> Res<Structure> {
    let d = db.dialect;
    // SHOW CREATE TABLE also answers for a view. PostgreSQL composes a table's text from what is read below.
    let create = async {
        if d == Dialect::Postgres {
            return Ok(None);
        }
        let pool = db.my_pool()?;
        let sql = d.create_stmt(false, schema, table);
        let t = std::time::Instant::now();
        let res = raw_sql(&sql).fetch_one(pool).await;
        db.note(schema, &sql, t, &res);
        res.map(|r| Some(SqlRow::My(r))).map_err(se)
    };
    let (cols, idx, fks, refs, trig, stat, create) = tokio::try_join!(
        query(db, d.columns(), schema, table),
        query(db, d.indexes(), schema, table),
        query(db, d.foreign_keys(), schema, table),
        query(db, d.referenced_by(), schema, table),
        query(db, d.triggers(), schema, table),
        query(db, d.table_status(), schema, table),
        create,
    )?;
    let mut st = Structure {
        columns: columns(d, &cols),
        indexes: indexes(&idx),
        foreign_keys: foreign_keys(&fks),
        referenced_by: foreign_keys(&refs),
        triggers: triggers(&trig),
        status: status(&stat),
        create: create.and_then(|r| text_at(&r, 1)).unwrap_or_default(),
        constraints: Vec::new(),
        pg_schema: (d == Dialect::Postgres).then(|| schema.to_string()),
        pg_deps: Vec::new(),
    };
    extras(db, schema, table, &mut st).await?;
    if d == Dialect::Postgres {
        let kind = stat.first().and_then(|r| text(r, "kind")).unwrap_or_default();
        pg_finish(db, schema, table, &kind, &mut st).await?;
    }
    Ok(st)
}

/// The two things a PostgreSQL table needs after the catalogs: its row count when the planner has none, and its CREATE
/// text. `kind` is the relkind: r table, p partitioned table, v view, m materialized view, f foreign table.
async fn pg_finish(db: &Db, schema: &str, table: &str, kind: &str, st: &mut Structure) -> Res<()> {
    let d = db.dialect;
    // A table nobody has analyzed has no estimate (-1 from 14 on, 0 on 13), so it is counted. A guard keeps a huge one
    // from holding the view up, and a foreign table is somebody else's server.
    if st.status.rows.is_none() && matches!(kind, "r" | "p" | "m") {
        let sql = format!("SELECT count(*)::text FROM {}", d.qualified(schema, table));
        if let Ok(rows) = db.read_text_with(schema, &super::pg::set_timeout("5s"), &sql, false).await {
            st.status.rows = rows.first().and_then(|r| super::worker::num_at(r, 0));
        }
    }
    if matches!(kind, "v" | "m") {
        let rows = db.read_text(schema, &d.create_stmt(true, schema, table), false).await?;
        let mut out = first_text(&rows);
        if !out.is_empty() {
            out = out.trim_end().to_string();
            if !out.ends_with(';') {
                out.push(';');
            }
        }
        // A materialized view keeps its own indexes.
        for ix in &st.indexes {
            out.push_str(&format!("\n{};", ix.def));
        }
        st.create = out;
    } else {
        st.create = pg_create_table(schema, table, st);
    }
    Ok(())
}

/// The CREATE TABLE of a PostgreSQL table, written from what the catalogs gave: the columns with their defaults,
/// identity and generated expressions, the primary key, unique, check and exclusion constraints, the foreign keys with
/// their referenced table spelled with its schema, then the indexes no constraint owns and the comments. Partition keys,
/// inheritance, storage options and ownership are not written. The dump of a later step reuses this.
pub fn pg_create_table(schema: &str, table: &str, st: &Structure) -> String {
    let q = |n: &str| Dialect::Postgres.quote_ident(n);
    let lit = |t: &str| super::value::lit(t, false);
    let name = Dialect::Postgres.qualified(schema, table);
    let mut lines: Vec<String> = Vec::new();
    for c in &st.columns {
        // A serial column's default points at a sequence the new table would not have, so it is written as serial.
        let serial = c.default_raw.as_deref().is_some_and(|d| d.starts_with("nextval(")).then_some(match c.full_type.as_str() {
            "integer" => Some("serial"),
            "bigint" => Some("bigserial"),
            "smallint" => Some("smallserial"),
            _ => None,
        });
        let mut l = format!("{} {}", q(&c.name), serial.flatten().unwrap_or(&c.full_type));
        if let Some(co) = &c.collation {
            l.push_str(&format!(" COLLATE {}", q(co)));
        }
        if let Some(at) = c.extra.find("GENERATED ALWAYS AS (") {
            l.push(' ');
            l.push_str(&c.extra[at..]);
        } else if c.extra.contains("GENERATED ALWAYS AS IDENTITY") {
            l.push_str(" GENERATED ALWAYS AS IDENTITY");
        } else if c.extra.contains("IDENTITY BY DEFAULT") {
            l.push_str(" GENERATED BY DEFAULT AS IDENTITY");
        } else if let (Some(def), None) = (&c.default_raw, serial.flatten()) {
            l.push_str(&format!(" DEFAULT {def}"));
        }
        if !c.nullable {
            l.push_str(" NOT NULL");
        }
        lines.push(l);
    }
    for k in &st.constraints {
        lines.push(format!("CONSTRAINT {} {}", q(&k.name), k.def));
    }
    for fk in &st.foreign_keys {
        let list = |v: &[String]| v.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
        let mut l = format!("CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})", q(&fk.name), list(&fk.cols), Dialect::Postgres.qualified(&fk.ref_db, &fk.ref_table), list(&fk.ref_cols));
        if fk.match_full {
            l.push_str(" MATCH FULL");
        }
        for (what, rule) in [("UPDATE", &fk.on_update), ("DELETE", &fk.on_delete)] {
            if !rule.is_empty() && rule != "NO ACTION" {
                l.push_str(&format!(" ON {what} {rule}"));
            }
        }
        if fk.deferrable {
            l.push_str(if fk.deferred { " DEFERRABLE INITIALLY DEFERRED" } else { " DEFERRABLE" });
        }
        if fk.not_valid {
            l.push_str(" NOT VALID");
        }
        lines.push(l);
    }
    let mut out = format!("CREATE TABLE {name} (\n    {}\n);", lines.join(",\n    "));
    for ix in st.indexes.iter().filter(|i| !i.constraint && !i.def.is_empty()) {
        out.push_str(&format!("\n{};", ix.def));
    }
    if !st.status.comment.is_empty() {
        out.push_str(&format!("\nCOMMENT ON TABLE {name} IS {};", lit(&st.status.comment)));
    }
    for c in st.columns.iter().filter(|c| !c.comment.is_empty()) {
        out.push_str(&format!("\nCOMMENT ON COLUMN {name}.{} IS {};", q(&c.name), lit(&c.comment)));
    }
    out
}

impl Worker {
    /// Everything the structure views show, in one answer. Uses the tab's `idx_req`, so it never
    /// competes with a page load.
    pub fn sql_structure(&self, req: u64, db: Db, schema: String, table: String) {
        self.spawn(async move { Msg::SqlStructure { req, res: load(&db, &schema, &table).await } });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_split_into_name_values_and_unsigned() {
        assert_eq!(split_type("int unsigned"), ("INT".into(), String::new(), true));
        assert_eq!(split_type("decimal(10,2) unsigned"), ("DECIMAL".into(), "10,2".into(), true));
        assert_eq!(split_type("varchar(255)"), ("VARCHAR".into(), "255".into(), false));
        assert_eq!(split_type("enum('a','b)c','d')"), ("ENUM".into(), "'a','b)c','d'".into(), false));
        assert_eq!(split_type("text"), ("TEXT".into(), String::new(), false));
        // An enum value that says unsigned is not the flag.
        assert!(!split_type("enum('unsigned')").2);
    }

    #[test]
    fn mariadb_defaults_read_like_mysql() {
        let m = Dialect::MariaDb;
        let n = |v: &str, nullable| normalize_default(m, Some(v.into()), nullable, "");
        assert_eq!(n("'abc'", false), (Some("abc".into()), false));
        assert_eq!(n("'it''s'", false), (Some("it's".into()), false));
        assert_eq!(n("''", false), (Some(String::new()), false));
        assert_eq!(n(r"'a\\b\'c'", false), (Some(r"a\b'c".into()), false));
        assert_eq!(n(r"'line\nbreak'", false), (Some("line\nbreak".into()), false));
        assert_eq!(n("NULL", true), (None, false));
        // A quoted string that says NULL is a string.
        assert_eq!(n("'NULL'", true), (Some("NULL".into()), false));
        assert_eq!(n("0.00", false), (Some("0.00".into()), false));
        assert_eq!(n("-1", false), (Some("-1".into()), false));
        assert_eq!(n("current_timestamp()", false), (Some("current_timestamp()".into()), true));
        assert_eq!(normalize_default(m, None, true, ""), (None, false));
    }

    #[test]
    fn postgres_defaults_read_like_mysql() {
        let n = |v: &str| normalize_default(Dialect::Postgres, Some(v.into()), true, "");
        assert_eq!(n("'pending'::character varying"), (Some("pending".into()), false));
        assert_eq!(n("'it''s'::text"), (Some("it's".into()), false));
        assert_eq!(n("''::text"), (Some(String::new()), false));
        assert_eq!(n(r"E'a\\b'::text"), (Some(r"a\b".into()), false));
        assert_eq!(n("'-5'::integer"), (Some("-5".into()), false));
        assert_eq!(n("'2024-01-01'::date"), (Some("2024-01-01".into()), false));
        assert_eq!(n("0"), (Some("0".into()), false));
        assert_eq!(n("-1.5"), (Some("-1.5".into()), false));
        assert_eq!(n("true"), (Some("true".into()), false));
        assert_eq!(n("NULL::text"), (None, false));
        assert_eq!(n("now()"), (Some("now()".into()), true));
        assert_eq!(n("CURRENT_TIMESTAMP"), (Some("CURRENT_TIMESTAMP".into()), true));
        assert_eq!(n("nextval('t_id_seq'::regclass)"), (Some("nextval('t_id_seq'::regclass)".into()), true));
        // A literal followed by an operator is an expression.
        assert_eq!(n("'a'::text || 'b'::text"), (Some("'a'::text || 'b'::text".into()), true));
        assert_eq!(normalize_default(Dialect::Postgres, None, true, ""), (None, false));
    }

    #[test]
    fn postgres_create_table_is_composed_from_the_catalogs() {
        let col = |name: &str, ty: &str, nullable: bool, raw: Option<&str>, extra: &str, comment: &str| Column { name: name.into(), full_type: ty.into(), nullable, default_raw: raw.map(Into::into), extra: extra.into(), comment: comment.into(), ..Default::default() };
        let st = Structure {
            columns: vec![
                col("id", "integer", false, None, "auto_increment IDENTITY BY DEFAULT", ""),
                col("status", "text", true, Some("'new'::text"), "", "State"),
                col("dbl", "integer", true, None, "GENERATED ALWAYS AS (id * 2) STORED", ""),
            ],
            indexes: vec![Index { name: "t_pkey".into(), constraint: true, def: "CREATE UNIQUE INDEX t_pkey ON public.t USING btree (id)".into(), ..Default::default() }, Index { name: "i".into(), def: "CREATE INDEX i ON public.t USING btree (status)".into(), ..Default::default() }],
            constraints: vec![Constraint { name: "t_pkey".into(), kind: 'p', def: "PRIMARY KEY (id)".into() }],
            foreign_keys: vec![ForeignKey { name: "fk".into(), cols: vec!["status".into()], ref_db: "other".into(), ref_table: "s".into(), ref_cols: vec!["code".into()], on_update: "NO ACTION".into(), on_delete: "SET NULL".into(), ..Default::default() }],
            status: Status { comment: "It's a table".into(), ..Default::default() },
            ..Default::default()
        };
        assert_eq!(
            pg_create_table("public", "t", &st),
            "CREATE TABLE \"public\".\"t\" (\n    \"id\" integer GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n    \"status\" text DEFAULT 'new'::text,\n    \"dbl\" integer GENERATED ALWAYS AS (id * 2) STORED,\n    CONSTRAINT \"t_pkey\" PRIMARY KEY (id),\n    CONSTRAINT \"fk\" FOREIGN KEY (\"status\") REFERENCES \"other\".\"s\" (\"code\") ON DELETE SET NULL\n);\nCREATE INDEX i ON public.t USING btree (status);\nCOMMENT ON TABLE \"public\".\"t\" IS 'It''s a table';\nCOMMENT ON COLUMN \"public\".\"t\".\"status\" IS 'State';"
        );
    }

    #[test]
    fn mysql_defaults_pass_through() {
        let d = Dialect::MySql;
        assert_eq!(normalize_default(d, Some("abc".into()), false, ""), (Some("abc".into()), false));
        assert_eq!(normalize_default(d, Some("CURRENT_TIMESTAMP".into()), false, "DEFAULT_GENERATED on update CURRENT_TIMESTAMP"), (Some("CURRENT_TIMESTAMP".into()), true));
        assert_eq!(normalize_default(d, None, true, ""), (None, false));
    }

    #[test]
    fn trigger_statement_is_composed() {
        let t = Trigger { name: "t1".into(), timing: "BEFORE".into(), event: "UPDATE".into(), body: "BEGIN\nEND".into(), ..Default::default() };
        assert_eq!(t.create_sql("orders"), "CREATE TRIGGER t1 BEFORE UPDATE ON orders\nFOR EACH ROW\nBEGIN\nEND");
    }

    #[test]
    fn fk_row_where_quotes_names_and_joins_with_and() {
        let fk = ForeignKey { ref_cols: vec!["a`b".into(), "c".into()], ..Default::default() };
        assert_eq!(fk.row_where(Dialect::MySql, &["1".into(), "'x'".into()]), "`a``b` = 1 AND `c` = 'x'");
    }

    #[test]
    fn fk_index_is_the_one_that_leads_with_its_columns() {
        let ix = |name: &str, cols: &[&str]| Index { name: name.into(), cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), ..Default::default() };
        let s = Structure { indexes: vec![ix("PRIMARY", &["id"]), ix("idx_a", &["x", "customer_id"]), ix("idx_customer", &["customer_id", "created_at"])], ..Default::default() };
        let fk = ForeignKey { cols: vec!["customer_id".into()], ..Default::default() };
        assert_eq!(s.index_for(&fk).map(|i| i.name.as_str()), Some("idx_customer"));
    }

    /// Needs servers: QUARRY_TEST_MYSQL (MySQL) and QUARRY_TEST_MARIADB (MariaDB), both like
    /// mysql://root:quarry@127.0.0.1:3308. Each is skipped when unset.
    fn live(var: &str) {
        let Ok(uri) = std::env::var(var) else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let pool = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            for sql in [
                "DROP DATABASE IF EXISTS quarry_struct",
                "CREATE DATABASE quarry_struct",
                "CREATE TABLE quarry_struct.customers (id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY, email VARCHAR(100) NOT NULL, UNIQUE KEY uq_email (email))",
                "CREATE TABLE quarry_struct.orders (
                    a INT NOT NULL, b INT NOT NULL,
                    customer_id INT UNSIGNED NOT NULL,
                    status ENUM('pending','paid','it''s') NOT NULL DEFAULT 'pending',
                    total DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00,
                    note TEXT NULL,
                    tag VARCHAR(20) NULL DEFAULT NULL,
                    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (a, b),
                    UNIQUE KEY uq_ab_cust (b, customer_id),
                    KEY idx_status_created (status, created_at DESC),
                    KEY idx_note (tag(5)),
                    CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES customers (id) ON UPDATE CASCADE ON DELETE RESTRICT
                ) COMMENT='Customer orders'",
                "CREATE TABLE quarry_struct.items (id INT PRIMARY KEY, oa INT NOT NULL, ob INT NOT NULL, CONSTRAINT fk_items_order FOREIGN KEY (oa, ob) REFERENCES orders (a, b) ON DELETE CASCADE)",
                "CREATE TRIGGER quarry_struct.orders_bu BEFORE UPDATE ON quarry_struct.orders FOR EACH ROW SET NEW.tag = 'x'",
            ] {
                sqlx::raw_sql(sql).execute(&p).await.unwrap_or_else(|err| panic!("{sql}: {err}"));
            }
            p
        });
        w.sql_connect(1, uri, std::time::Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, ..)), .. } = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!("expected SqlConnected") };
        w.sql_structure(7, db.clone(), "quarry_struct".into(), "orders".into());
        let Msg::SqlStructure { req: 7, res } = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!("expected SqlStructure") };
        let st = res.unwrap_or_else(|err| panic!("{var}: {err}"));

        let names: Vec<_> = st.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "customer_id", "status", "total", "note", "tag", "created_at"]);
        let col = |n: &str| st.columns.iter().find(|c| c.name == n).unwrap();
        assert_eq!(col("a").key, "PRI");
        assert_eq!(col("customer_id").key, "MUL");
        assert!(col("customer_id").unsigned() && col("customer_id").base_type() == "INT");
        assert_eq!((col("status").base_type().as_str(), col("status").args().as_str()), ("ENUM", "'pending','paid','it''s'"));
        assert_eq!(col("status").default, Some("pending".into()), "{var} enum default");
        assert_eq!(col("total").args(), "10,2");
        assert!(col("total").unsigned());
        assert_eq!(col("total").default, Some("0.00".into()));
        assert!(col("note").nullable && col("note").default.is_none(), "{var} text NULL default");
        assert!(col("tag").nullable && col("tag").default.is_none(), "{var} explicit NULL default");
        assert!(col("created_at").default_expr && col("created_at").default.as_deref().is_some_and(|d| d.to_ascii_lowercase().starts_with("current_timestamp")), "{:?}", col("created_at"));
        assert!(!col("status").default_expr);

        let ix: Vec<_> = st.indexes.iter().map(|i| (i.name.as_str(), i.unique, i.cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>())).collect();
        assert_eq!(ix[0], ("PRIMARY", true, vec!["a", "b"]), "{ix:?}");
        let by = |n: &str| st.indexes.iter().find(|i| i.name == n).unwrap();
        assert!(by("uq_ab_cust").unique);
        assert!(!by("idx_status_created").unique);
        assert_eq!(by("idx_status_created").cols.iter().map(|c| c.desc).collect::<Vec<_>>(), [false, true]);
        assert_eq!(by("idx_note").cols[0].prefix, Some(5));
        assert_eq!(by("PRIMARY").kind, "BTREE");

        assert_eq!(st.foreign_keys.len(), 1);
        let fk = &st.foreign_keys[0];
        assert_eq!((fk.name.as_str(), fk.cols.as_slice(), fk.ref_table.as_str(), fk.ref_cols.as_slice()), ("fk_orders_customer", &["customer_id".to_string()][..], "customers", &["id".to_string()][..]));
        assert_eq!((fk.on_update.as_str(), fk.on_delete.as_str()), ("CASCADE", "RESTRICT"));
        assert!(st.index_for(fk).is_some());
        // A composite key from another table is one entry with both columns.
        assert_eq!(st.referenced_by.len(), 1);
        let r = &st.referenced_by[0];
        assert_eq!((r.table.as_str(), r.cols.as_slice(), r.ref_cols.as_slice()), ("items", &["oa".to_string(), "ob".to_string()][..], &["a".to_string(), "b".to_string()][..]));
        assert_eq!(r.on_delete, "CASCADE");

        assert_eq!(st.triggers.len(), 1);
        let t = &st.triggers[0];
        assert_eq!((t.name.as_str(), t.timing.as_str(), t.event.as_str()), ("orders_bu", "BEFORE", "UPDATE"));
        assert!(t.body.contains("NEW.tag"), "{}", t.body);

        assert_eq!(st.status.engine, "InnoDB");
        assert_eq!(st.status.comment, "Customer orders");
        assert!(st.status.created.starts_with("20"), "{}", st.status.created);
        assert!(st.create.starts_with("CREATE TABLE `orders`"), "{}", st.create);

        // The customers table's keys point the other way.
        w.sql_structure(8, db, "quarry_struct".into(), "customers".into());
        let Msg::SqlStructure { res, .. } = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!() };
        let c = res.unwrap();
        assert!(c.foreign_keys.is_empty());
        assert_eq!(c.referenced_by.len(), 1);
        assert!(c.indexes.iter().any(|i| i.name == "uq_email" && i.unique));
        assert!(c.columns[0].extra.contains("auto_increment"));

        w.rt.block_on(async { sqlx::raw_sql("DROP DATABASE quarry_struct").execute(&pool).await.unwrap() });
    }

    #[test]
    fn live_mysql() {
        live("QUARRY_TEST_MYSQL");
    }

    #[test]
    fn live_mariadb() {
        live("QUARRY_TEST_MARIADB");
    }
}
