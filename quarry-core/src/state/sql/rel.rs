//! State behind src/app/sql/rel.rs, which draws it.

use crate::db::sql::ddl::{self, Change};
use crate::db::sql::structure::{ForeignKey, Structure};
use crate::db;
use crate::state::sql::alter::PlanMemo;

/// What the orphan count depends on, as `ddl::orphan_key` names it.
#[derive(Clone, PartialEq)]
pub struct OrphanKey {
    pub cols: Vec<String>,
    pub ref_db: String,
    pub ref_table: String,
    pub ref_cols: Vec<String>,
}

impl OrphanKey {
    pub fn of(fk: &ForeignKey) -> Self {
        let (cols, ref_db, ref_table, ref_cols) = ddl::orphan_key(fk);
        Self { cols, ref_db, ref_table, ref_cols }
    }
}

/// The Add relation dialog.
pub struct FkForm {
    pub name: String,
    /// The name was typed, so it stops following the columns.
    pub named: bool,
    /// Column of this table and column it points to, in key order. Empty means not picked yet.
    pub pairs: Vec<(String, String)>,
    pub ref_db: String,
    pub ref_table: String,
    pub on_update: String,
    pub on_delete: String,
    pub own: Structure,
    /// The columns of `own` the staged changes leave, for the pickers. Neither input changes while the dialog is open.
    pub own_cols: Vec<String>,
    pub rest: Vec<Change>,
    /// The plan of the key as it stands, kept while the changes and the table stay as they are.
    pub memo: Option<PlanMemo>,
    pub parent: Option<Structure>,
    /// The schema and table `parent` was asked for, and the request that carries the answer.
    pub asked: (String, String),
    pub parent_req: u64,
    pub parent_err: Option<String>,
    /// How many rows point at nothing for the key as it stands, and which columns and request that is for.
    pub orphans: Option<Result<u64, String>>,
    pub orphan_for: Option<OrphanKey>,
    pub orphan_req: u64,
    /// The server takes a key only on a whole primary or unique key, and the request that asks it.
    pub strict: bool,
    pub rules_req: u64,
    /// The person asked for the check on a big table.
    pub check: bool,
    /// A few of the rows the count found, as their key values.
    pub sample: Vec<String>,
    /// PostgreSQL: the namespace is a schema of the tab's database, and these are the options only it has.
    pub pg: bool,
    pub match_full: bool,
    pub deferrable: bool,
    pub deferred: bool,
    pub not_valid: bool,
    /// The key being edited, which a change replaces (PostgreSQL). None for a new one.
    pub from: Option<ForeignKey>,
}

impl FkForm {
    pub fn parent_loaded(&mut self, req: u64, res: db::Res<Structure>) -> bool {
        if req != self.parent_req || req == 0 {
            return false;
        }
        match res {
            Ok(s) => (self.parent, self.parent_err) = (Some(s), None),
            Err(e) => (self.parent, self.parent_err) = (None, Some(e)),
        }
        true
    }

    pub fn rules_done(&mut self, req: u64, strict: bool) {
        if req == self.rules_req && req != 0 {
            self.strict = strict;
        }
    }

    pub fn orphans_done(&mut self, req: u64, res: db::Res<u64>, sample: Vec<String>) -> bool {
        if req != self.orphan_req || req == 0 {
            return false;
        }
        (self.orphans, self.sample) = (Some(res), sample);
        true
    }

    /// Puts the form back in the state where the count of the rows with no match is asked and not answered yet.
    #[cfg(any(test, feature = "test-support"))]
    pub fn forget_orphan_answer(&mut self) {
        self.orphans = None;
    }

    /// The key the form describes once every part is picked.
    pub fn key(&self, schema: &str) -> Option<ForeignKey> {
        if self.ref_table.is_empty() || self.pairs.is_empty() || self.pairs.iter().any(|(a, b)| a.is_empty() || b.is_empty()) {
            return None;
        }
        Some(ForeignKey {
            name: self.name.trim().to_string(),
            db: schema.to_string(),
            table: String::new(),
            cols: self.pairs.iter().map(|p| p.0.clone()).collect(),
            ref_db: self.ref_db.clone(),
            ref_table: self.ref_table.clone(),
            ref_cols: self.pairs.iter().map(|p| p.1.clone()).collect(),
            on_update: self.on_update.clone(),
            on_delete: self.on_delete.clone(),
            match_full: self.pg && self.match_full,
            deferrable: self.pg && self.deferrable,
            deferred: self.pg && self.deferrable && self.deferred,
            not_valid: self.pg && self.not_valid,
            comment: self.from.as_ref().map(|o| o.comment.clone()).unwrap_or_default(),
            // The parent as the form saw it, so the transaction can refuse when it changed before the key was added.
            ref_seen: self.parent.as_ref().filter(|p| self.pg && p.pg_schema.is_some()).map(|p| ddl::ref_state(p, &self.pairs.iter().map(|x| x.1.clone()).collect::<Vec<_>>())).unwrap_or_default(),
            ..Default::default()
        })
    }
}
