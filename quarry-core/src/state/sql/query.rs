//! State behind src/app/sql/query.rs, which draws it.

use std::ops::Range;
use crate::db;
use crate::db::sql::lex;
use crate::state::SqlTab;
use std::collections::HashMap;

/// The live check of the Query editor. Answers are kept by statement text for the current target, so moving the caret
/// back to a checked statement asks nothing and every statement keeps its mark.
#[derive(Default)]
pub struct LintState {
    /// The connection, database and schema the answers in `seen` were read in; a change clears them.
    pub target: String,
    /// The statement in flight and its request, 0 when none.
    pub asked: String,
    pub req: u64,
    /// The statement under the caret and when it last changed, until typing has rested for `LINT_PAUSE`.
    pub wait: Option<(String, f64)>,
    pub seen: HashMap<String, Seen>,
}

/// One answer of the live check, Err when the check could not run: it marks nothing and is asked again after `LINT_RETRY`.
pub struct Seen {
    pub lint: Result<Option<db::sql::Lint>, ()>,
    pub at: std::time::Instant,
}

impl LintState {
    /// What the check found in `stmt`, if it marked anything.
    pub fn mark(&self, stmt: &str) -> Option<&db::sql::Lint> {
        self.seen.get(stmt).and_then(|s| s.lint.as_ref().ok()?.as_ref())
    }

    /// Whether `stmt` needs asking: never asked, or a check that could not run long enough ago.
    pub fn stale(&self, stmt: &str) -> bool {
        self.seen.get(stmt).is_none_or(|s| s.lint.is_err() && s.at.elapsed() >= LINT_RETRY)
    }

    /// Forgets the unknown-name warnings, once a run or a reread schema may have made what they named.
    pub fn drop_warnings(&mut self) {
        self.seen.retain(|_, s| !matches!(&s.lint, Ok(Some(l)) if l.warn));
    }

    pub fn put(&mut self, stmt: String, lint: Result<Option<db::sql::Lint>, ()>) {
        self.seen.insert(stmt, Seen { lint, at: std::time::Instant::now() });
    }
}

/// Where the caret stands: line and column, and the statement under it.
#[derive(Clone)]
pub struct CaretInfo {
    pub key: (usize, u64, usize),
    pub line_col: (usize, usize),
    pub stmt: Option<(Range<usize>, usize, usize)>,
    /// Every statement's byte range, kept so the gutter does not lex the script again each frame.
    pub stmts: std::sync::Arc<Vec<Range<usize>>>,
}

/// How long a check that could not run (no connection, a lock) waits before the statement is asked again.
pub const LINT_RETRY: std::time::Duration = std::time::Duration::from_secs(10);

impl SqlTab {
    /// The result set shown, clamped to the ones there are.
    pub fn cur_set(&self) -> Option<&db::sql::ResultSet> {
        self.sets.get(self.set_sel.min(self.sets.len().saturating_sub(1))).map(|s| &**s)
    }

    /// The caret's place in the script. Lexing the script is costly next to a frame, so the answer is kept
    /// until the text or the caret changes.
    pub fn caret_info(&self, d: db::sql::Dialect) -> CaretInfo {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        let key = (self.query.len(), h.finish(), self.caret);
        let mut slot = self.caret_cache.borrow_mut();
        match &*slot {
            Some(c) if c.key == key => c.clone(),
            _ => {
                let stmts: Vec<Range<usize>> = lex::split_statements(&self.query, d).into_iter().map(|(r, _)| r).collect();
                let c = CaretInfo { key, line_col: line_col(&self.query, self.caret), stmt: statement_in(&stmts, &self.query, self.caret, d), stmts: std::sync::Arc::new(stmts) };
                *slot = Some(c.clone());
                c
            }
        }
    }
}

/// 1-based line and column of a caret given as a char index.
pub fn line_col(text: &str, caret: usize) -> (usize, usize) {
    text.chars().take(caret).fold((1, 1), |(l, c), ch| if ch == '\n' { (l + 1, 1) } else { (l, c + 1) })
}

/// `statement_pos` with the statements of `text` already split.
pub fn statement_in(all: &[Range<usize>], text: &str, caret: usize, d: db::sql::Dialect) -> Option<(Range<usize>, usize, usize)> {
    let r = lex::statement_at(text, byte_at(text, caret), d)?;
    let i = all.iter().position(|a| *a == r)?;
    Some((r, i + 1, all.len()))
}

pub fn byte_at(text: &str, caret: usize) -> usize {
    text.char_indices().nth(caret).map_or(text.len(), |(b, _)| b)
}

/// The database a run uses: the picked one, else the tab's own.
pub fn run_db(t: &crate::state::Tab) -> String {
    t.sql.as_ref().map(|s| s.run_db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| t.sel.db.clone())
}

/// The schema a PostgreSQL Query view runs in: the tab's own run schema (restored or set by a library script), else the tab's own when the run is in
/// the tab's own database. Another database has other schemas, so the tab's schema is never carried over to it and the
/// answer stays empty until that database's schemas arrive.
pub fn run_schema_choice(typed: &str, run_db: &str, sel_db: &str, sel_schema: &str) -> String {
    let typed = typed.trim();
    if !typed.is_empty() {
        typed.to_string()
    } else if run_db.is_empty() || run_db == sel_db {
        sel_schema.to_string()
    } else {
        String::new()
    }
}

pub fn run_schema_of(t: &crate::state::Tab) -> String {
    run_schema_choice(t.sql.as_ref().map_or("", |s| s.run_schema.as_str()), t.sql.as_ref().map_or("", |s| s.run_db.as_str()), &t.sel.db, &t.sel.schema)
}

/// The database a run connects to and the namespace inside it. A PostgreSQL connection is bound to one database, so
/// the `Db` points at it and the namespace is the tab's schema; MySQL picks the database per statement. Export of a
/// result uses it too, so the statement reads the same tables it ran against.
pub fn run_target(t: &crate::state::Tab, db: db::sql::Db) -> (db::sql::Db, String) {
    let (rd, rs) = t.sql.as_ref().map_or(("", ""), |s| (s.run_db.as_str(), s.run_schema.as_str()));
    target_of(t, db, rd, rs)
}

/// `run_target` of the run whose results are shown, for statements read again from them.
pub fn ran_target(t: &crate::state::Tab, db: db::sql::Db) -> (db::sql::Db, String) {
    match t.sql.as_ref().and_then(|s| s.ran_in.as_ref()) {
        Some((rd, rs)) => target_of(t, db, rd, rs),
        None => run_target(t, db),
    }
}

/// The target for a picked database `rd` (empty for the tab's own) and typed schema `rs`.
fn target_of(t: &crate::state::Tab, db: db::sql::Db, rd: &str, rs: &str) -> (db::sql::Db, String) {
    let d = if rd.is_empty() { t.sel.db.clone() } else { rd.to_string() };
    if db.dialect == db::sql::Dialect::Postgres {
        return (db.at(&d), run_schema_choice(rs, rd, &t.sel.db, &t.sel.schema));
    }
    (db, d)
}

/// The key each column of a result goes by in its column choices. A join can return two columns of one name, so the
/// second and later ones carry their position after a separator the SQL names cannot hold.
pub fn result_keys(cols: &[db::sql::Col]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(cols.len());
    for (i, c) in cols.iter().enumerate() {
        out.push(if out.contains(&c.name) { format!("{}\u{1}{i}", c.name) } else { c.name.clone() });
    }
    out
}
