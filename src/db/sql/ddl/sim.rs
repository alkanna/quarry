//! Simulating staged changes on a structure: the table they leave, the statements they make and the undo of each.

use super::*;

/// The table as the changes leave it, tracked while their inverses are collected. A column whose definition
/// cannot be read back keeps the reason.
#[derive(Clone)]
pub(in crate::db::sql) struct Sim {
    pub(in crate::db::sql) cols: Vec<(String, Result<ColDef, String>)>,
    pub(in crate::db::sql) idx: Vec<IndexDef>,
    pub(in crate::db::sql) fks: Vec<ForeignKey>,
    pub(in crate::db::sql) checks: Vec<CheckConstraint>,
    /// PostgreSQL drops a column together with every index that holds it, puts a new column last and cannot place one.
    pg: bool,
    /// PostgreSQL: what the server drops with each column, as read from the catalog.
    pub(in crate::db::sql) deps: Vec<crate::db::sql::structure::ColDep>,
    /// What the inverses collected so far give up, for the undo's confirmation.
    pub(in crate::db::sql) notes: Vec<String>,
}

impl Sim {
    pub(in crate::db::sql) fn of(st: &Structure) -> Sim {
        Sim { cols: st.columns.iter().map(|c| (c.name.clone(), ColDef::in_table(st, c))).collect(), idx: st.indexes.iter().map(IndexDef::from_index).collect(), fks: st.foreign_keys.clone(), checks: st.checks.clone(), pg: st.is_pg(), deps: st.pg_deps.clone(), notes: Vec::new() }
    }

    /// An index the server can use for a key on these columns: it starts with them, in order, whole.
    fn serves(&self, cols: &[String]) -> bool {
        self.idx.iter().any(|ix| matches!(ix.kind, IndexKind::Index | IndexKind::Unique | IndexKind::Primary) && ix.cols.len() >= cols.len() && ix.cols.iter().zip(cols).all(|(a, b)| a.prefix.is_none() && same_name(&a.name, b)))
    }

    /// Where a foreign key is. PostgreSQL keeps `fk` and `FK` as two constraints of one table, so a name is matched exactly
    /// there; MySQL compares them without case.
    pub(in crate::db::sql) fn fk_at(&self, name: &str) -> Option<usize> {
        self.fks.iter().position(|k| fk_name_is(self.pg, &k.name, name))
    }

    /// Where a CHECK is. PostgreSQL tells `c` from `C`; MySQL and MariaDB compare constraint names without case.
    pub(in crate::db::sql) fn check_at(&self, name: &str) -> Option<usize> {
        self.checks.iter().position(|k| fk_name_is(self.pg, &k.name, name))
    }

    /// The index to drop along with a key: one named like the key on exactly its columns is what the server
    /// (or this app) made for it. Another key that relies on it keeps it.
    fn backing_index(&self, name: &str) -> Option<String> {
        let fk = &self.fks[self.fk_at(name)?];
        let ix = self.idx.iter().find(|ix| ix.kind == IndexKind::Index && same_name(&ix.name, &fk.name) && ix.cols.len() == fk.cols.len() && ix.cols.iter().zip(&fk.cols).all(|(a, b)| a.prefix.is_none() && !a.desc && same_name(&a.name, b)))?;
        let shared = self.fks.iter().any(|o| !same_name(&o.name, &fk.name) && o.cols.len() <= ix.cols.len() && ix.cols.iter().zip(&o.cols).all(|(a, b)| same_name(&a.name, b)));
        (!shared).then(|| ix.name.clone())
    }

    pub(in crate::db::sql) fn at(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|(n, _)| same_name(n, name))
    }

    /// Where a column is now, as the clause that puts it back there.
    fn pos_of(&self, i: usize) -> Pos {
        if i == 0 { Pos::First } else { Pos::After(self.cols[i - 1].0.clone()) }
    }

    fn insert(&mut self, col: (String, Result<ColDef, String>), pos: &Pos) {
        let at = match pos {
            _ if self.pg => self.cols.len(),
            Pos::Keep => self.cols.len(),
            Pos::First => 0,
            Pos::After(c) => self.at(c).map_or(self.cols.len(), |i| i + 1),
        };
        self.cols.insert(at.min(self.cols.len()), col);
    }

    /// The server renames a column in the indexes and foreign keys that hold it.
    fn rename_in_indexes(&mut self, from: &str, to: &str) {
        for ix in &mut self.idx {
            for c in ix.cols.iter_mut().filter(|c| same_name(&c.name, from)) {
                c.name = to.into();
            }
        }
        for k in &mut self.fks {
            for c in k.cols.iter_mut().filter(|c| same_name(c, from)) {
                *c = to.into();
            }
        }
    }

    /// Applies one change and returns the changes that undo it, in the order to run them. On PostgreSQL a change that is
    /// refused leaves the table as it was, so a caller that goes on after the refusal does not read a half-applied one.
    pub(in crate::db::sql) fn apply(&mut self, c: &Change) -> Result<Vec<Change>, String> {
        if !self.pg {
            return self.apply_in_place(c);
        }
        let mut work = self.clone();
        let back = work.apply_in_place(c)?;
        *self = work;
        Ok(back)
    }

    fn apply_in_place(&mut self, c: &Change) -> Result<Vec<Change>, String> {
        match c {
            Change::Add { def, pos } => {
                self.insert((def.name.clone(), Ok(def.clone())), pos);
                Ok(vec![Change::Drop(def.name.clone())])
            }
            Change::Modify { from, def, pos } => {
                let i = self.at(from).ok_or_else(|| format!("Column {from} is not in the table."))?;
                let old_pos = self.pos_of(i);
                let (_, old) = self.cols.remove(i);
                if *pos == Pos::Keep {
                    self.cols.insert(i, (def.name.clone(), Ok(def.clone())));
                } else {
                    self.insert((def.name.clone(), Ok(def.clone())), pos);
                }
                if from != &def.name {
                    self.rename_in_indexes(from, &def.name);
                    for d in self.deps.iter_mut().filter(|d| same_name(&d.col, from)) {
                        d.col = def.name.clone();
                    }
                }
                let back_pos = if *pos == Pos::Keep { Pos::Keep } else { old_pos };
                Ok(vec![Change::Modify { from: def.name.clone(), def: old?, pos: back_pos }])
            }
            Change::Drop(name) if self.pg => {
                let i = self.at(name).ok_or_else(|| format!("Column {name} is not in the table."))?;
                // The server drops the table's own foreign keys that hold the column, and refuses to drop a column that a
                // key of another table points at, so only these have to come back.
                let (fks_gone, fks_keep): (Vec<ForeignKey>, Vec<ForeignKey>) = std::mem::take(&mut self.fks).into_iter().partition(|k| k.cols.iter().any(|c| same_name(c, name)));
                self.fks = fks_keep;
                for k in &fks_gone {
                    if !k.opaque.is_empty() {
                        return Err(format!("Dropping {name} also drops the foreign key {}, which this editor cannot put back: {}.", k.name, k.opaque));
                    }
                    // The column returns empty, and MATCH FULL refuses a key that is partly NULL.
                    if k.match_full && k.cols.len() > 1 {
                        return Err(format!("Dropping {name} also drops the foreign key {}, which is MATCH FULL over several columns: it could not be added back with this column empty.", k.name));
                    }
                }
                let (_, old) = self.cols.remove(i);
                let old = old?;
                // Every index and constraint that holds the column goes whole, so undo adds each one back as it was.
                let (gone, keep): (Vec<IndexDef>, Vec<IndexDef>) = std::mem::take(&mut self.idx).into_iter().partition(|ix| ix.cols.iter().any(|c| same_name(&c.name, name)));
                self.idx = keep;
                // The server also drops what depends on the column without the column being in an index's key parts: a
                // partial index that only names it in its condition, an expression or INCLUDE index, a CHECK, a trigger.
                // Undo restores only the indexes it can write and the column's own default and sequence.
                let lost: Vec<&str> = self.deps.iter().filter(|d| same_name(&d.col, name)).filter(|d| match d.kind.as_str() {
                    "index" => !gone.iter().any(|ix| same_name(&ix.name, &d.name) && !ix.functional),
                    "key" => !gone.iter().any(|ix| same_name(&ix.name, &d.name) && !ix.functional && ix.constraint),
                    "default" => false,
                    "sequence" => !old.auto_increment,
                    "constraint" => !(d.own && fks_gone.iter().any(|k| k.name == d.name)),
                    _ => true,
                }).map(|d| d.what.as_str()).collect();
                if !lost.is_empty() {
                    return Err(format!("Dropping {name} also drops {}, which this editor cannot put back. A foreign key of another table that points at the column makes the server refuse the drop instead.", lost.join(", ")));
                }
                // A NOT NULL column with nothing to fill the rows with cannot come back NOT NULL on a table that has rows,
                // and a primary key needs it, so the column returns nullable and without its primary key.
                let blank = !old.nullable && !old.auto_increment && !matches!(old.default, Dv::Value(_) | Dv::Expr(_));
                let (skipped, gone): (Vec<IndexDef>, Vec<IndexDef>) = gone.into_iter().partition(|ix| blank && ix.kind == IndexKind::Primary);
                let back_def = if blank { ColDef { nullable: true, default: Dv::Null, ..old } } else { old };
                if blank {
                    self.notes.push(format!("Undo adds {name} back empty and allowing NULL{}: it has no default to fill the rows with, so NOT NULL could not be set.", if skipped.is_empty() { "" } else { ", without its primary key" }));
                }
                // The column comes back last: PostgreSQL has no column order to restore it to.
                let mut back = vec![Change::Add { def: back_def, pos: Pos::Keep }];
                // A key of this table on its own key needs that key back first, and it is not when the primary key is skipped.
                for k in fks_gone.iter().filter(|k| same_name(&k.ref_table, &k.table) && same_name(&k.ref_db, &k.db)) {
                    let covered = |ix: &IndexDef| matches!(ix.kind, IndexKind::Unique | IndexKind::Primary) && !ix.functional && ix.cols.len() == k.ref_cols.len() && ix.cols.iter().all(|c| k.ref_cols.iter().any(|r| same_name(r, &c.name)));
                    if !gone.iter().any(covered) && !self.idx.iter().any(covered) {
                        return Err(format!("Dropping {name} also drops the foreign key {}, which points at a key of this table that cannot come back with it.", k.name));
                    }
                }
                back.extend(gone.into_iter().map(Change::AddIndex));
                back.extend(fks_gone.into_iter().map(Change::AddFk));
                Ok(back)
            }
            Change::Drop(name) => {
                let i = self.at(name).ok_or_else(|| format!("Column {name} is not in the table."))?;
                let pos = self.pos_of(i);
                let (_, old) = self.cols.remove(i);
                // The server shrinks every index that holds the column and drops those left empty, and undo has to
                // bring them back as they were.
                let mut back = vec![Change::Add { def: old?, pos }];
                let mut keep = Vec::new();
                for mut ix in std::mem::take(&mut self.idx) {
                    if !ix.cols.iter().any(|c| same_name(&c.name, name)) {
                        keep.push(ix);
                        continue;
                    }
                    let whole = ix.clone();
                    ix.cols.retain(|c| !same_name(&c.name, name));
                    if !ix.cols.is_empty() {
                        back.push(Change::DropIndex(ix.name.clone()));
                        keep.push(ix);
                    }
                    back.push(Change::AddIndex(whole));
                }
                self.idx = keep;
                Ok(back)
            }
            Change::AddIndex(def) => {
                self.idx.push(def.clone());
                if self.pg {
                    if let Some(last) = self.idx.last_mut() {
                        last.constraint |= last.kind == IndexKind::Primary;
                    }
                    // A primary key makes its columns NOT NULL and dropping it does not undo that, so undo says it too.
                    let mut back = vec![Change::DropIndex(def.name.clone())];
                    if def.kind == IndexKind::Primary {
                        for ic in &def.cols {
                            if let Some(i) = self.at(&ic.name) && let Ok(old) = &self.cols[i].1.clone() && old.nullable {
                                self.cols[i].1 = Ok(ColDef { nullable: false, ..old.clone() });
                                back.push(Change::Modify { from: old.name.clone(), def: old.clone(), pos: Pos::Keep });
                            }
                        }
                    }
                    return Ok(back);
                }
                Ok(vec![Change::DropIndex(if def.kind == IndexKind::Primary { "PRIMARY".into() } else { def.name.clone() })])
            }
            Change::DropIndex(name) => {
                let i = self.idx.iter().position(|x| same_name(&x.name, name) || (name == "PRIMARY" && x.kind == IndexKind::Primary)).ok_or_else(|| format!("Index {name} is not on the table."))?;
                // PostgreSQL's own text of the index puts it back exactly, unless a constraint owns it.
                let x = &self.idx[i];
                if x.functional && !(self.pg && x.raw.starts_with("CREATE ") && !x.constraint && x.kind != IndexKind::Primary) {
                    return Err(format!("Index {name} has an expression part, which cannot be read back to add it again."));
                }
                Ok(vec![Change::AddIndex(self.idx.remove(i))])
            }
            Change::RenameIndex { from, to, def } => {
                let i = self.idx.iter().position(|x| same_name(&x.name, from)).ok_or_else(|| format!("Index {from} is not on the table."))?;
                self.idx[i].name = to.clone();
                Ok(vec![Change::RenameIndex { from: to.clone(), to: from.clone(), def: IndexDef { name: to.clone(), ..def.clone() } }])
            }
            Change::AddFk(k) => {
                if self.fk_at(&k.name).is_some() {
                    return Err(format!("The table already has a foreign key called {}.", k.name));
                }
                self.fks.push(k.clone());
                Ok(vec![Change::DropFk(k.name.clone())])
            }
            Change::DropFk(name) => {
                let i = self.fk_at(name).ok_or_else(|| format!("Foreign key {name} is not on the table."))?;
                if !self.fks[i].opaque.is_empty() {
                    return Err(format!("Foreign key {name} cannot be written back: {}.", self.fks[i].opaque));
                }
                Ok(vec![Change::AddFk(self.fks.remove(i))])
            }
            Change::RenameFk { from, to } => {
                let i = self.fk_at(from).ok_or_else(|| format!("Foreign key {from} is not on the table."))?;
                if !fk_name_is(self.pg, from, to) && self.fk_at(to).is_some() {
                    return Err(format!("The table already has a foreign key called {to}."));
                }
                self.fks[i].name = to.clone();
                Ok(vec![Change::RenameFk { from: to.clone(), to: from.clone() }])
            }
            Change::SetFkDeferral { name, deferrable, deferred } => {
                let i = self.fk_at(name).ok_or_else(|| format!("Foreign key {name} is not on the table."))?;
                let k = &mut self.fks[i];
                let back = Change::SetFkDeferral { name: k.name.clone(), deferrable: k.deferrable, deferred: k.deferred };
                (k.deferrable, k.deferred) = (*deferrable, *deferred);
                Ok(vec![back])
            }
            Change::AddCheck(k) => {
                if self.check_at(&k.name).is_some() {
                    return Err(format!("The table already has a check constraint called {}.", k.name));
                }
                self.checks.push(k.clone());
                Ok(vec![Change::DropCheck(k.name.clone())])
            }
            Change::DropCheck(name) => {
                let i = self.check_at(name).ok_or_else(|| format!("Check constraint {name} is not on the table."))?;
                Ok(vec![Change::AddCheck(self.checks.remove(i))])
            }
        }
    }
}

/// The column names after the changes, for the position lists and for moving a column.
pub fn columns_after(st: &Structure, changes: &[Change]) -> Vec<String> {
    let mut sim = Sim::of(st);
    for c in changes {
        let _ = sim.apply(c);
    }
    sim.cols.into_iter().map(|(n, _)| n).collect()
}

/// The index names after the changes, for the duplicate check of the index form.
pub fn indexes_after(st: &Structure, changes: &[Change]) -> Vec<String> {
    let mut sim = Sim::of(st);
    for c in changes {
        let _ = sim.apply(c);
    }
    sim.idx.into_iter().map(|i| i.name).collect()
}

/// The CHECK names after the changes, for the duplicate check of the CHECK form.
pub fn checks_after(st: &Structure, changes: &[Change]) -> Vec<String> {
    let mut sim = Sim::of(st);
    for c in changes {
        let _ = sim.apply(c);
    }
    sim.checks.into_iter().map(|k| k.name).collect()
}

/// The name a column ends up with once `later` ran.
pub(super) fn final_name(name: &str, later: &[Change]) -> String {
    later.iter().fold(name.to_string(), |n, c| match c {
        Change::Modify { from, def, .. } if same_name(from, &n) => def.name.clone(),
        _ => n,
    })
}

/// The changes with the index edits the engine will not do by itself. MariaDB refuses to drop a column that an
/// index holds, so each such index is dropped first and added again without the column (or left out when the
/// column was its only one). A new foreign key gets an index of its own when none serves its columns, written
/// out so the preview shows it, and dropping a key takes the index made for it along.
pub fn expand(d: Dialect, st: &Structure, changes: &[Change]) -> Vec<Change> {
    let mut sim = Sim::of(st);
    let mut out = Vec::new();
    for (k, c) in changes.iter().enumerate() {
        match c {
            Change::Drop(col) if !d.drops_index_with_column() => {
                for ix in sim.idx.iter().filter(|ix| ix.cols.iter().any(|c| same_name(&c.name, col))) {
                    out.push(Change::DropIndex(if ix.kind == IndexKind::Primary { "PRIMARY".into() } else { ix.name.clone() }));
                    let mut rest = ix.clone();
                    rest.cols.retain(|c| !same_name(&c.name, col));
                    // The server reads the columns of an added index by the names the whole ALTER leaves, so a column a
                    // later change renames goes in by its new name.
                    for ic in &mut rest.cols {
                        ic.name = final_name(&ic.name, &changes[k + 1..]);
                    }
                    if !rest.cols.is_empty() {
                        out.push(Change::AddIndex(rest));
                    }
                }
            }
            // PostgreSQL does not make an index for a key, and does not need one to hold it.
            Change::AddFk(fk) if d != Dialect::Postgres && !sim.serves(&fk.cols) => {
                let taken = |n: &str| sim.idx.iter().any(|x| same_name(&x.name, n));
                let name = if taken(&fk.name) { format!("{}_idx", fk.name) } else { fk.name.clone() };
                let ix = IndexDef { name, kind: IndexKind::Index, cols: fk.cols.iter().map(|c| IdxCol { name: c.clone(), prefix: None, desc: false }).collect(), ..Default::default() };
                let _ = sim.apply(&Change::AddIndex(ix.clone()));
                out.push(Change::AddIndex(ix));
            }
            _ => {}
        }
        let backing = if let (Change::DropFk(n), true) = (c, d != Dialect::Postgres) { sim.backing_index(n) } else { None };
        let _ = sim.apply(c);
        out.push(c.clone());
        if let Some(ix) = backing {
            let _ = sim.apply(&Change::DropIndex(ix.clone()));
            out.push(Change::DropIndex(ix));
        }
    }
    out
}

/// What an applied structure change needs to be undone: the statements, what running them would lose, and
/// what the table looked like right after the change, which must still hold when the undo runs.
#[derive(Clone, Debug, PartialEq)]
pub struct AlterUndo {
    /// One ALTER per inverse step, run in order. One statement could not do it: the server reads the columns of
    /// an added index by the names the statement leaves and drops columns before it places them, so a step
    /// that names a column the next one renames or drops would fail.
    pub steps: Vec<String>,
    /// What the change did, for the Recent changes list.
    pub label: String,
    /// What running the undo loses (a column it drops, a type it converts back), for its typed confirmation.
    pub loss: Vec<String>,
    /// The sql_mode the steps' literals are written for.
    pub mode: Mode,
    /// The columns, indexes, foreign keys and CHECKs the steps touch.
    pub cols: Vec<String>,
    pub idx: Vec<String>,
    pub fks: Vec<String>,
    pub checks: Vec<String>,
    /// `snapshot` of those right after the change ran, filled by the worker.
    pub expect: Vec<(String, Option<String>)>,
    /// PostgreSQL: the other tables the steps lock and rely on, and the keys they add whose rows are checked first.
    /// `seen` of a table is filled by the worker, from how the table looked right after the change ran.
    pub refs: Vec<RefCheck>,
    pub orphans: Vec<ForeignKey>,
    /// The inverse changes `steps` were written from, one group per step, kept so the steps can be written again
    /// when the session's sql_mode changes before the undo runs.
    pub groups: Vec<Vec<Change>>,
}

impl AlterUndo {
    /// The steps as the session in mode `m` must read them: the stored ones, or the same inverse written again.
    pub(in crate::db::sql) fn steps_for(&self, d: Dialect, version: &str, table: &str, m: Mode) -> Result<Vec<String>, String> {
        // PostgreSQL's statements do not depend on the session's quoting.
        if m == self.mode || d == Dialect::Postgres {
            return Ok(self.steps.clone());
        }
        self.groups.iter().map(|g| alter(d, version, m, table, g)).collect()
    }
}

/// The staged changes as they run, and their undo or why there is none. The preview and Apply both use it, so
/// what is shown is what runs.
#[derive(Debug)]
pub struct Plan {
    pub undo: Result<AlterUndo, String>,
    /// The statements that run, one per entry. MySQL has one ALTER; PostgreSQL has one per clause, all in one transaction.
    pub steps: Vec<String>,
    /// PostgreSQL: what the touched columns and indexes looked like when the plan was made. The transaction reads them
    /// again after it holds the lock and refuses to run when they differ. None on MySQL.
    pub expect: Option<Expect>,
}

impl Plan {
    /// The statements as the preview shows them. MySQL has the one ALTER as it is; PostgreSQL has a script of its steps.
    pub fn sql(&self) -> String {
        if self.expect.is_none() {
            return self.steps.join("\n\n");
        }
        let (now, later) = crate::db::sql::pg::ddl::split_steps(&self.steps);
        if later.is_empty() {
            return script(&self.steps);
        }
        let (now, later): (Vec<String>, Vec<String>) = (now.into_iter().cloned().collect(), later.into_iter().cloned().collect());
        let note = "-- CREATE INDEX CONCURRENTLY cannot run inside a transaction block, so the statements below run on their own after the ones above. If one fails, the ones above stay applied.";
        if now.is_empty() { format!("{note}\n\n{}", script(&later)) } else { format!("{}\n\n{note}\n\n{}", script(&now), script(&later)) }
    }
}

/// The names a plan reads and `snapshot` of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Expect {
    pub cols: Vec<String>,
    pub idx: Vec<String>,
    pub fks: Vec<String>,
    pub checks: Vec<String>,
    pub snap: Vec<(String, Option<String>)>,
    /// PostgreSQL: every index, constraint and dependent of the table when the plan was made, for a plan (a column drop)
    /// whose undo relies on the table having none besides these. A name list cannot see one that another session added.
    pub all: Option<Vec<String>>,
    /// PostgreSQL: the type texts the plan carries from the person, which the server must resolve as types.
    pub types: Vec<String>,
    /// PostgreSQL: the other tables the plan locks and relies on (the parents of the keys it adds or drops).
    pub refs: Vec<RefCheck>,
    /// PostgreSQL: the keys it adds, whose existing rows are checked under the lock so the person sees the ones that fail.
    pub orphans: Vec<ForeignKey>,
    /// PostgreSQL: the changes take no more than SHARE ROW EXCLUSIVE on the changed table (they only add keys or change when a
    /// key is checked), where any other change takes ACCESS EXCLUSIVE.
    pub own_share: bool,
}

/// PostgreSQL: a table other than the one being changed that a plan locks. A key that is added relies on the referenced
/// columns and the unique indexes there (`seen` is how they looked, `pg::ddl::ref_state`), and a key that is dropped only
/// needs the lock, which PostgreSQL takes on both tables of a key.
#[derive(Clone, Debug, PartialEq)]
pub struct RefCheck {
    pub schema: String,
    pub table: String,
    pub cols: Vec<String>,
    /// A key is being dropped: ACCESS EXCLUSIVE. Adding one needs SHARE ROW EXCLUSIVE.
    pub exclusive: bool,
    pub seen: Option<Vec<String>>,
}

pub fn plan(d: Dialect, version: &str, m: Mode, table: &str, st: &Structure, staged: &[Change]) -> Result<Plan, String> {
    let changes = expand(d, st, staged);
    if d == Dialect::Postgres {
        return crate::db::sql::pg::ddl::plan(table, st, &changes);
    }
    let sql = alter(d, version, m, table, &changes)?;
    Ok(Plan { steps: vec![sql], undo: undo_of(d, version, m, table, st, &changes), expect: None })
}

pub(super) fn undo_of(d: Dialect, version: &str, m: Mode, table: &str, before: &Structure, changes: &[Change]) -> Result<AlterUndo, String> {
    let mut sim = Sim::of(before);
    let mut groups = Vec::new();
    for c in changes {
        groups.push(sim.apply(c)?);
    }
    groups.reverse();
    let steps = groups.iter().map(|g| alter(d, version, m, table, g)).collect::<Result<Vec<_>, _>>()?;
    let loss = undo_loss(&sim, &groups);
    let (cols, idx, fks, checks) = touched(&groups);
    Ok(AlterUndo { steps, label: String::new(), loss, mode: m, cols, idx, fks, checks, expect: Vec::new(), refs: Vec::new(), orphans: Vec::new(), groups })
}

/// The statements that put the table back, from the structure read before the changes ran. Err says why
/// there are none.
#[cfg(test)]
pub fn inverse(d: Dialect, version: &str, m: Mode, table: &str, before: &Structure, changes: &[Change]) -> Result<Vec<String>, String> {
    undo_of(d, version, m, table, before, changes).map(|u| u.steps)
}

/// A new definition converts the values a column holds.
pub(super) fn converts(old: &ColDef, new: &ColDef) -> bool {
    old.ty != new.ty || old.args != new.args || old.unsigned != new.unsigned || (old.nullable && !new.nullable)
}

pub(super) fn type_text(c: &ColDef) -> String {
    format!("{}{}{}", c.ty, if c.args.is_empty() { String::new() } else { format!("({})", c.args) }, if c.unsigned { " UNSIGNED" } else { "" })
}

/// What the undo steps destroy, run on the table the change left.
pub(in crate::db::sql) fn undo_loss(after: &Sim, groups: &[Vec<Change>]) -> Vec<String> {
    let mut sim = after.clone();
    let mut out = after.notes.clone();
    for c in groups.iter().flatten() {
        match c {
            Change::Drop(n) => out.push(format!("Undo drops column {n} and every value it holds.")),
            Change::Modify { from, def, .. } => {
                if let Some(Ok(cur)) = sim.at(from).map(|i| &sim.cols[i].1) {
                    if converts(cur, def) {
                        out.push(format!("Undo converts {from} back to {}, which can cut or reject the values it holds now.", type_text(def)));
                    }
                    // Dropping an identity drops its sequence, so the undo starts a new one at the largest value the column holds.
                    if sim.pg && def.auto_increment && !cur.auto_increment {
                        out.push(format!("Undo makes {from} an auto increment again on a new sequence that continues after its largest value: the old sequence's step, limits, cache and position are not restored."));
                    }
                }
            }
            _ => {}
        }
        let _ = sim.apply(c);
    }
    out
}

/// The column, index, foreign key and CHECK names the undo steps read or write, with no repeats.
pub(in crate::db::sql) fn touched(groups: &[Vec<Change>]) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let (mut cols, mut idx, mut fks, mut checks): (Vec<String>, Vec<String>, Vec<String>, Vec<String>) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    fn add(v: &mut Vec<String>, n: &str) {
        if !v.iter().any(|x| same_name(x, n)) {
            v.push(n.to_string());
        }
    }
    // A key name is kept as typed: PostgreSQL has `fk` and `FK` side by side, and both have to be read back.
    fn add_exact(v: &mut Vec<String>, n: &str) {
        if !v.iter().any(|x| x == n) {
            v.push(n.to_string());
        }
    }
    for c in groups.iter().flatten() {
        let pos = match c {
            Change::Add { def, pos } => {
                add(&mut cols, &def.name);
                Some(pos)
            }
            Change::Modify { from, def, pos } => {
                add(&mut cols, from);
                add(&mut cols, &def.name);
                Some(pos)
            }
            Change::Drop(n) => {
                add(&mut cols, n);
                None
            }
            Change::AddIndex(ix) => {
                add(&mut idx, &ix.name);
                for ic in &ix.cols {
                    add(&mut cols, &ic.name);
                }
                None
            }
            Change::DropIndex(n) => {
                add(&mut idx, n);
                None
            }
            Change::RenameIndex { from, to, .. } => {
                add(&mut idx, from);
                add(&mut idx, to);
                None
            }
            Change::AddFk(k) => {
                add_exact(&mut fks, &k.name);
                for c in &k.cols {
                    add(&mut cols, c);
                }
                None
            }
            Change::DropFk(n) => {
                add_exact(&mut fks, n);
                None
            }
            Change::RenameFk { from, to } => {
                add_exact(&mut fks, from);
                add_exact(&mut fks, to);
                None
            }
            Change::SetFkDeferral { name, .. } => {
                add_exact(&mut fks, name);
                None
            }
            Change::AddCheck(k) => {
                add_exact(&mut checks, &k.name);
                None
            }
            Change::DropCheck(n) => {
                add_exact(&mut checks, n);
                None
            }
        };
        if let Some(Pos::After(n)) = pos {
            add(&mut cols, n);
        }
    }
    (cols, idx, fks, checks)
}

/// A CHECK in one comparable line, with everything the editor can write back.
pub(in crate::db::sql) fn check_line(k: &CheckConstraint) -> String {
    format!("{}|{}|{}|{}|{}", k.name, k.expr, k.not_valid, k.no_inherit, k.not_enforced)
}

/// A foreign key in one comparable line, with everything the editor can write back.
pub(in crate::db::sql) fn fk_line(k: &ForeignKey) -> String {
    format!("{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}", k.name, k.cols.join(","), k.ref_db, k.ref_table, k.ref_cols.join(","), k.on_update, k.on_delete, k.match_full, k.deferrable, k.deferred, k.not_valid, k.comment)
}

/// PostgreSQL: the referenced columns of a parent table and its unique indexes, in one comparable list. A key that is added
/// leans on both, so the plan keeps how they looked and the transaction compares them again under the lock.
pub fn ref_state(st: &Structure, cols: &[String]) -> Vec<String> {
    let mut v: Vec<String> = cols.iter().map(|n| match st.columns.iter().find(|c| same_name(&c.name, n)) {
        Some(c) => format!("column {}|{}|{:?}", c.name, c.full_type, c.collation),
        None => format!("column {n}|missing"),
    }).collect();
    v.extend(st.indexes.iter().filter(|i| i.unique).map(|i| format!("unique {}|{}|{}|{}", i.name, i.def, i.deferrable, i.deferred)));
    v
}

/// How the server shows each named column, index and foreign key, or None when it is not there. Two snapshots are
/// equal when nothing an undo relies on changed in between. Every part is compared exactly, table names too: both
/// snapshots come from the same server, which spells a name the same way each time whatever lower_case_table_names is.
pub fn snapshot(st: &Structure, cols: &[String], idx: &[String], fks: &[String], checks: &[String]) -> Vec<(String, Option<String>)> {
    let col = |n: &String| {
        let c = st.columns.iter().find(|c| same_name(&c.name, n));
        (format!("column {n}"), c.map(|c| format!("{}|{}|{}|{:?}|{}|{}|{:?}|{}|{:?}|{:?}{}", c.name, c.full_type, c.nullable, c.default, c.default_expr, c.extra, c.collation, c.comment, c.srid, c.check, c.default_raw.as_ref().map_or(String::new(), |r| format!("|{r}")))))
    };
    let index = |n: &String| {
        let i = st.indexes.iter().find(|i| same_name(&i.name, n));
        (format!("index {n}"), i.map(|i| format!("{}|{}|{}|{}|{}|{}{}", i.name, i.unique, i.kind, i.comment, i.invisible, i.cols.iter().map(|c| format!("{}:{:?}:{}:{:?}", c.name, c.prefix, c.desc, c.expr)).collect::<Vec<_>>().join(","), if i.def.is_empty() { String::new() } else { format!("|{}|{}|{}|{}|{}|{}|{}", i.def, i.constraint, i.primary, i.complex, i.deferrable, i.deferred, i.con_comment) })))
    };
    let fk = |n: &String| {
        let k = st.foreign_keys.iter().find(|k| fk_name_is(st.is_pg(), &k.name, n));
        (format!("foreign key {n}"), k.map(fk_line))
    };
    let check = |n: &String| {
        let k = st.checks.iter().find(|k| fk_name_is(st.is_pg(), &k.name, n));
        (format!("check {n}"), k.map(check_line))
    };
    cols.iter().map(col).chain(idx.iter().map(index)).chain(fks.iter().map(fk)).chain(checks.iter().map(check)).collect()
}

/// What the staged changes cost that undo cannot give back. Shown in the preview.
pub fn warnings(before: &Structure, changes: &[Change]) -> Vec<String> {
    let mut out = Vec::new();
    let mut fk_sim = Sim::of(before);
    let sim = Sim::of(before);
    for c in changes {
        if let Change::DropFk(n) = c && let Some(ix) = fk_sim.backing_index(n).filter(|_| !before.is_pg()) {
            out.push(format!("Dropping {n} also drops the index {ix}, which was made for it. Undo brings both back."));
        }
        if let (Change::AddFk(k), Some(_)) = (c, &before.pg_schema) {
            if !fk_sim.serves(&k.cols) {
                out.push(format!("PostgreSQL does not index the columns of a foreign key. Without an index on ({}), deleting or changing a row of {} scans this table.", k.cols.join(", "), k.ref_table));
            }
            if k.not_valid {
                out.push(format!("{} is added NOT VALID: the rows already in the table are not checked, and undo brings back the same unchecked key.", k.name));
            }
        }
        if let (Change::AddCheck(k), true) = (c, before.is_pg()) {
            if k.not_valid {
                out.push(format!("{} is added NOT VALID: the rows already in the table are not checked, and undo brings back the same unchecked constraint.", k.name));
            } else if before.status.rows.is_none_or(|r| r > 0) {
                out.push(format!("PostgreSQL checks every row of the table for {} while it holds an exclusive lock, and refuses the constraint if one fails. On a big table add it NOT VALID instead.", k.name));
            }
        }
        let _ = fk_sim.apply(c);
        match c {
            Change::Drop(n) if before.is_pg() => {
                let blank = sim.at(n).and_then(|i| sim.cols[i].1.as_ref().ok()).is_some_and(|d| !d.nullable && !d.auto_increment && !matches!(d.default, Dv::Value(_) | Dv::Expr(_)));
                let back = if blank { "allowing NULL and without its primary key, with its other indexes." } else { "with its indexes." };
                out.push(format!("Dropping {n} deletes its values for good, with every index and constraint that uses it. Undo brings the column back empty and last, {back}"));
            }
            Change::Add { def, .. } if before.is_pg() && !def.auto_increment && before.status.rows.is_some_and(|r| r > 0) => {
                if let Dv::Expr(x) = &def.default && crate::db::sql::pg::ddl::calls_function(x) {
                    out.push(format!("The default of {} calls a function. Any function call is read as volatile here, since the editor does not look the function up. A volatile one runs once for each of the table's rows while PostgreSQL holds an exclusive lock on the table, and again on every later insert.", def.name));
                }
            }
            Change::Drop(n) => out.push(format!("Dropping {n} deletes its values for good. Undo brings the column back empty.")),
            Change::Modify { from, def, .. } => {
                if let Some(Ok(old)) = sim.at(from).map(|i| &sim.cols[i].1) && converts(old, def) {
                    out.push(format!("Changing {from} converts its values and can cut or reject some. Undo restores the definition, not the values."));
                }
            }
            Change::DropIndex(n) if n == "PRIMARY" || before.indexes.iter().any(|i| i.primary && i.name == *n) => out.push("Dropping the primary key makes the rows read-only here until the table has a key again.".into()),
            _ => {}
        }
    }
    out
}

/// True when the changes lose data that undo cannot restore (so Apply asks to be confirmed).
pub fn drops_data(changes: &[Change]) -> bool {
    changes.iter().any(|c| matches!(c, Change::Drop(_)))
}

/// Why an index of this kind cannot be built on these columns, as (name, type) pairs.
pub fn index_problem(kind: IndexKind, cols: &[(String, String)]) -> Option<String> {
    let ty = |c: &(String, String)| split_type(&c.1).0;
    match kind {
        IndexKind::Fulltext if cols.iter().any(|c| !matches!(ty(c).as_str(), "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT")) => Some("FULLTEXT works on text columns only.".into()),
        IndexKind::Spatial if cols.len() != 1 || !is_geometry(&ty(&cols[0])) => Some("SPATIAL takes one geometry column.".into()),
        _ => None,
    }
}

/// Dedupes names for the "Duplicate" action: `name_copy`, `name_copy2`, and so on.
pub fn free_name(taken: &[String], base: &str) -> String {
    let set: HashSet<String> = taken.iter().map(|t| t.to_lowercase()).collect();
    let first = format!("{base}_copy");
    if !set.contains(&first.to_lowercase()) {
        return first;
    }
    (2..).map(|n| format!("{first}{n}")).find(|n| !set.contains(&n.to_lowercase())).unwrap_or(first)
}
