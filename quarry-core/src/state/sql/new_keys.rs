//! State behind src/app/sql/new_keys.rs, which draws it.

use crate::db::sql::ddl;
use crate::db::sql::pg::create::{Check, NewIndex, Unique};
use crate::db::sql::structure::{ForeignKey, Structure};
use crate::db::sql::Dialect;
use crate::db;

/// A foreign key of the builder, with the parent's structure as the server gave it, read on a request of its own like the Add
/// relation dialog's. `key.cols` and `key.ref_cols` are kept the same length, one pair per line, empty until picked.
#[derive(Clone)]
pub struct NewFk {
    pub key: ForeignKey,
    /// The key points at the table being made, which follows the name typed.
    pub own: bool,
    pub parent: Option<Structure>,
    /// The schema and table `parent` was asked for, and the request that carries the answer.
    pub asked: (String, String),
    pub req: u64,
    pub err: Option<String>,
}

impl NewFk {
    pub fn new(schema: &str, d: Dialect) -> Self {
        let action = if d == Dialect::Postgres { "NO ACTION" } else { "RESTRICT" };
        let key = ForeignKey { ref_db: schema.to_string(), cols: vec![String::new()], ref_cols: vec![String::new()], on_update: action.into(), on_delete: action.into(), ..Default::default() };
        NewFk { key, own: false, parent: None, asked: Default::default(), req: 0, err: None }
    }
}

/// What the builder collects besides the columns.
#[derive(Default)]
pub struct NewKeys {
    /// The UNIQUE constraints over several columns; one over a single column is also its column's Unique box on PostgreSQL.
    pub uniques: Vec<Unique>,
    pub checks: Vec<Check>,
    pub fks: Vec<NewFk>,
    pub indexes: Vec<NewIndex>,
}

impl NewKeys {
    pub fn is_empty(&self) -> bool {
        self.uniques.is_empty() && self.checks.is_empty() && self.fks.is_empty() && self.indexes.is_empty()
    }

    /// Every pick of a column by name in the UNIQUEs, the indexes and the keys (the parent side only when the key points at this
    /// table), so a rename or a removal reaches all of them.
    pub fn picks_mut(&mut self) -> impl Iterator<Item = &mut String> {
        let lists = self.uniques.iter_mut().map(|u| &mut u.cols).chain(self.indexes.iter_mut().map(|i| &mut i.cols));
        let keys = self.fks.iter_mut().flat_map(|k| {
            let own = k.own;
            k.key.cols.iter_mut().chain(k.key.ref_cols.iter_mut().filter(move |_| own))
        });
        lists.flat_map(|v| v.iter_mut()).chain(keys)
    }

    /// A column was renamed from `from`; its picks follow, unless another column still has that name (`names_after` are the table's
    /// column names now) and the picks may be its. An emptied name is a removal, because an empty pick on a key line means nothing
    /// picked.
    pub fn renamed<'a>(&mut self, names_after: impl IntoIterator<Item = &'a str>, from: &str, to: &str) {
        if from.is_empty() || names_after.into_iter().any(|n| n == from) {
            return;
        }
        if to.is_empty() {
            return self.forget(from);
        }
        self.picks_mut().filter(|p| *p == from).for_each(|p| *p = to.to_string());
    }

    /// A column was removed; its picks go, unless another column still has that name (`names_after` are the table's column names now).
    pub fn removed<'a>(&mut self, names_after: impl IntoIterator<Item = &'a str>, gone: &str) {
        if !names_after.into_iter().any(|n| n == gone) {
            self.forget(gone);
        }
    }

    /// The keys as the spec has them: one that points at the table itself is written with the table's own schema and name.
    pub fn fk_keys(&self, schema: &str, name: &str) -> Vec<ForeignKey> {
        self.fks.iter().map(|k| {
            let mut key = k.key.clone();
            if k.own {
                (key.ref_db, key.ref_table) = (schema.to_string(), name.to_string());
            }
            key
        }).collect()
    }

    /// Takes a removed column out of every list. A UNIQUE or index pick list hides a name it does not offer, so a stale one could
    /// neither be seen nor removed and a new column of that name would silently take its place. A key line is kept and left unpicked.
    pub fn forget(&mut self, name: &str) {
        for v in self.uniques.iter_mut().map(|u| &mut u.cols).chain(self.indexes.iter_mut().map(|i| &mut i.cols)) {
            v.retain(|c| c != name);
        }
        self.picks_mut().filter(|p| *p == name).for_each(String::clear);
    }

    /// The answer to a parent's structure read, when it is for one of these keys.
    pub fn parent_loaded(&mut self, req: u64, res: &db::Res<Structure>) -> bool {
        let Some(k) = self.fks.iter_mut().find(|k| k.req == req && req != 0) else { return false };
        match res {
            Ok(s) => (k.parent, k.err) = (Some(s.clone()), None),
            Err(e) => (k.parent, k.err) = (None, Some(e.clone())),
        }
        true
    }

    /// The first thing stopping a key that the statements cannot see: a part not picked, a parent not read yet or one that does not
    /// fit. The second value is true while a parent is still loading, which is not a mistake to point at. `own` is the table being
    /// made as `fks` (the same keys, as the spec has them) would find it. With an `engine` (MySQL) a parent must be InnoDB too, which the
    /// server only says as "cannot add a foreign key constraint", and a key at the table itself is checked too. That the new table
    /// itself is InnoDB is `create::spec_problem`'s rule, so it is said once.
    pub fn key_problem(&self, fks: &[ForeignKey], own: &Structure, table: &str, whole_key: bool, engine: Option<&str>) -> Option<(String, bool)> {
        for (i, (k, key)) in self.fks.iter().zip(fks).enumerate() {
            let n = i + 1;
            if key.ref_table.is_empty() {
                return Some((format!("Pick the table foreign key {n} points at."), false));
            }
            if key.cols.iter().chain(&key.ref_cols).any(|c| c.is_empty()) {
                return Some((format!("Pick both columns of every line of foreign key {n}."), false));
            }
            if k.own {
                if engine.is_some() && let Some(why) = ddl::fk_problem(own, table, own, key, whole_key) {
                    return Some((format!("Foreign key {n}: {why}"), false));
                }
                continue;
            }
            if let Some(e) = &k.err {
                return Some((format!("Could not read {}: {e}", key.ref_table), false));
            }
            match &k.parent {
                None => return Some((format!("Reading {}...", key.ref_table), true)),
                Some(p) => {
                    if engine.is_some() && !p.status.engine.is_empty() && !p.status.engine.eq_ignore_ascii_case("InnoDB") {
                        return Some((format!("Foreign key {n}: {} is a {} table, and a foreign key can only point at an InnoDB table.", key.ref_table, p.status.engine), false));
                    }
                    if let Some(why) = ddl::fk_problem(own, table, p, key, whole_key) {
                        return Some((format!("Foreign key {n}: {why}"), false));
                    }
                }
            }
        }
        None
    }
}
