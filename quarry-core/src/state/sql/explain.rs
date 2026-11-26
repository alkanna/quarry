//! State behind src/app/sql/explain.rs, which draws it.

use super::explain_compare::Prev;
use super::explain_flame::FlameCache;
use super::explain_hypo::HypoRun;
use super::explain_tree::TreeRow;
use crate::db::sql::plan::{self, compare, Analyze, Plan};
use crate::db::sql::{Db, Dialect};
use std::sync::Arc;

/// The state of one open SQL explain dialog.
pub struct SqlExplain {
    /// The id of the request in flight, so an old answer never fills a newer dialog.
    pub req: u64,
    pub busy: bool,
    pub stmt: String,
    /// The statement as the session's plan history knows it, normalized once.
    pub key_stmt: String,
    pub conn: u64,
    /// The tab's database, for the Query tab the classic rows open in.
    pub tab_db: String,
    /// The database the plan was made in: the run picker's when one is picked, else the tab's.
    pub database: String,
    pub dialect: Dialect,
    pub db: Db,
    pub schema: String,
    /// Shared with the session's plan history, which keeps it for the next Compare.
    pub plan: Option<Arc<Plan>>,
    /// What each tree row shows and the flame chart's bars, worked out once per plan and its findings.
    pub tree_rows: Option<Vec<TreeRow>>,
    pub flame: Option<FlameCache>,
    /// Why the plan could not be drawn, when the server's text could not be read.
    pub unread: Option<String>,
    pub raw: String,
    /// The node the panel shows, and one the tree should scroll to.
    pub sel: Option<usize>,
    pub scroll_to: Option<usize>,
    /// The findings strip shows all of them, not the first two.
    pub more: bool,
    pub analyzed: bool,
    /// The statement ran inside a transaction that was rolled back.
    pub rolled_back: bool,
    /// How ANALYZE may run this statement, or why it may not.
    pub analyze: Result<Analyze, String>,
    /// What the request in flight asked for.
    pub asked: Option<Analyze>,
    /// Stop was pressed, so the answer that follows is no error to show.
    pub stopped: bool,
    /// The head tab.
    pub pane: Pane,
    /// The flame chart is as wide as each node's time, not its cost.
    pub flame_time: bool,
    /// The last plan of this statement this session, when it differs from this one.
    pub prev: Option<Prev>,
    /// The catalog's evidence for the index suggestions, kept across an ANALYZE rerun of the same statement, and the
    /// request reading it while it reads, apart from `req` so a rerun does not drop it.
    pub ev: Option<plan::Evidence>,
    pub ev_req: Option<u64>,
    /// The offline demo and the tests: made-up evidence instead of a catalog read.
    pub demo: bool,
    /// The connection is locked, as of the last frame drawn.
    pub locked: bool,
    /// Why the statement cannot be tried with HypoPG, for the lock state it was checked under. Lexing it every frame
    /// would cost more than the dialog.
    pub hypo_refused: Option<(bool, Option<String>)>,
    /// The HypoPG trial of a candidate index: the request in flight and the candidate it tries, its answer, and whether
    /// Compare shows the trial's plan instead of the last one. Apart from `req`, like `ev_req`.
    pub hypo_wait: Option<(u64, String)>,
    pub hypo: Option<HypoRun>,
    pub hypo_cmp: bool,
}

/// The head tabs: the views of a readable plan and the server's text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Tree,
    Flame,
    Compare,
    Raw,
}

impl Pane {
    pub fn name(self) -> &'static str {
        match self {
            Pane::Tree => "Tree",
            Pane::Flame => "Flame",
            Pane::Compare => "Compare",
            Pane::Raw => "Raw",
        }
    }
}

impl SqlExplain {
    #[allow(clippy::too_many_arguments)]
    pub fn new(stmt: String, conn: u64, tab_db: String, database: String, dialect: Dialect, db: Db, schema: String, analyze: Result<Analyze, String>) -> Self {
        let key_stmt = compare::normalize(&stmt, dialect);
        SqlExplain { req: 0, busy: false, stmt, key_stmt, conn, tab_db, database, dialect, db, schema, plan: None, tree_rows: None, flame: None, hypo_refused: None, unread: None, raw: String::new(), sel: None, scroll_to: None, more: false, analyzed: false, rolled_back: false, analyze, asked: None, stopped: false, pane: Pane::Tree, flame_time: false, prev: None, ev: None, ev_req: None, demo: false, locked: true, hypo_wait: None, hypo: None, hypo_cmp: false }
    }

    /// Drops what was worked out from the plan or its findings, so the next frame works it out again.
    pub fn plan_changed(&mut self) {
        (self.tree_rows, self.flame) = (None, None);
    }

    /// Compare has nothing to show once neither a last plan nor a trial is set against this one.
    pub fn leave_empty_compare(&mut self) {
        if self.pane == Pane::Compare && self.prev.is_none() && !self.hypo_cmp {
            self.pane = Pane::Tree;
        }
    }
}
