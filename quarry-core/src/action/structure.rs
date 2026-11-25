//! The Structure view of a SQL tab: its staged ALTER of columns and indexes, Apply, and the answer that clears or keeps
//! it. Drawn by src/app/sql/structure.rs and alter.rs; the dialogs that stage a column, an index, a key or a check stay
//! in the app with their forms.

use super::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::sql::ddl::{self, Change, ColDef, Mode, Plan, Pos};
use crate::db::sql::structure::Structure;
use crate::db::sql::Dialect;
use crate::db::{self, UndoAction};
use crate::state::sql::alter::*;
use crate::state::sql::structure::Load;
use crate::state::Danger;
use serde::{Deserialize, Serialize};

pub mod forms;
pub mod trig;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StructA {
    /// Stages dropping column `col`.
    DropColumn { tab: u64, col: String },
    /// Stages column `col` moved to where column `to` is, as the grid shows them.
    MoveColumn { tab: u64, col: String, to: String },
    /// Stages dropping index `index`.
    DropIndex { tab: u64, index: String },
    /// Removes staged change `at` from the list.
    Unstage { tab: u64, at: usize },
    Discard { tab: u64 },
    /// Apply: confirms first when the change loses data or cannot be undone.
    Apply { tab: u64 },
    /// Runs the staged ALTER, once confirmed.
    Run { tab: u64 },
    /// Reads the structure again.
    Refresh { tab: u64 },
    /// Opens the column dialog on column `col`, or on a new column.
    EditColumn { tab: u64, col: Option<String> },
    /// Opens the column dialog on a copy of column `col`.
    DuplicateColumn { tab: u64, col: String },
    /// Opens the index dialog on index `index`, or on a new index.
    EditIndex { tab: u64, index: Option<String> },
    /// Opens the CHECK dialog on constraint `check`, or on a new one.
    EditCheck { tab: u64, check: Option<String> },
    DropCheck { tab: u64, check: String },
    /// Opens the relation dialog on foreign key `fk`, or on a new one.
    EditFk { tab: u64, fk: Option<String> },
    DropFk { tab: u64, fk: String },
    /// Opens the table options dialog.
    EditTable { tab: u64 },
    /// A dialog's Stage: its own changes go after the staged list as it is now; `replaces` is the column whose staged edit it
    /// took over.
    Stage { tab: u64, changes: Vec<Change>, replaces: Option<String> },
    /// The table options dialog's Stage: its changes replace every staged table option.
    StageTable { tab: u64, changes: Vec<Change> },
    /// Triggers: a draft of a new trigger.
    TrigNew { tab: u64 },
    /// Triggers: drops the edits of draft `key` (a trigger's name, or empty for the new one).
    TrigRevert { tab: u64, key: String },
    /// Triggers: saves draft `key`.
    TrigSave { tab: u64, key: String },
    /// Triggers: asks before dropping trigger `name`.
    TrigAskDrop { tab: u64, name: String },
    /// Triggers: drops trigger `name`, once confirmed.
    TrigDrop { tab: u64, name: String },
    /// Info: a maintenance statement; the heavy ones ask first.
    Maintain { tab: u64, what: crate::db::sql::Maint },
    /// Info: runs a maintenance statement, once confirmed.
    RunMaint { tab: u64, what: crate::db::sql::Maint },
}

impl StructA {
    fn tab(&self) -> u64 {
        match self {
            StructA::DropColumn { tab, .. } | StructA::MoveColumn { tab, .. } | StructA::DropIndex { tab, .. } | StructA::Unstage { tab, .. } | StructA::Discard { tab } | StructA::Apply { tab } | StructA::Run { tab } | StructA::Refresh { tab } => *tab,
            StructA::EditColumn { tab, .. } | StructA::DuplicateColumn { tab, .. } | StructA::EditIndex { tab, .. } | StructA::EditCheck { tab, .. } | StructA::DropCheck { tab, .. } | StructA::EditFk { tab, .. } | StructA::DropFk { tab, .. } | StructA::EditTable { tab } | StructA::Stage { tab, .. } | StructA::StageTable { tab, .. } => *tab,
            StructA::TrigNew { tab } | StructA::TrigRevert { tab, .. } | StructA::TrigSave { tab, .. } | StructA::TrigAskDrop { tab, .. } | StructA::TrigDrop { tab, .. } | StructA::Maintain { tab, .. } | StructA::RunMaint { tab, .. } => *tab,
        }
    }
}

impl Core {
    pub(super) fn structure_act(&mut self, a: StructA) -> Result<Reply, Refusal> {
        if self.tabs.get(self.cur).is_none_or(|t| t.uid != a.tab() || t.sql.is_none()) {
            return Err(self.refuse("That tab is no longer open."));
        }
        let col = |c: &Core, name: &str| c.structure().and_then(|s| s.columns.iter().position(|x| x.name == name));
        match a {
            StructA::DropColumn { col: name, .. } => {
                if let Some(i) = col(self, &name) {
                    self.stage_drop_column(i);
                }
            }
            StructA::MoveColumn { col: name, to, .. } => {
                if let (Some(i), Some(j)) = (col(self, &name), col(self, &to)) {
                    self.stage_move_column_to(i, j);
                }
            }
            StructA::DropIndex { index, .. } => {
                if let Some(i) = self.structure().and_then(|s| s.indexes.iter().position(|x| x.name == index)) {
                    self.stage_drop_index(i);
                }
            }
            StructA::Unstage { at, .. } => self.unstage(at),
            StructA::Discard { .. } => self.discard_staged(),
            StructA::Apply { .. } => return self.request_alter_apply(),
            StructA::Run { .. } => self.run_alter(),
            StructA::Refresh { .. } => {
                if let Some(s) = self.tabs[self.cur].sql.as_mut() {
                    s.st.load = Load::Idle;
                }
                self.request_structure();
            }
            StructA::EditColumn { col, .. } => self.open_col_form(col.as_deref()),
            StructA::DuplicateColumn { col, .. } => self.duplicate_column(&col),
            StructA::EditIndex { index, .. } => self.open_idx_form(index.as_deref()),
            StructA::EditCheck { check, .. } => self.open_check_form(check.as_deref()),
            StructA::DropCheck { check, .. } => self.stage_drop_check(&check),
            StructA::EditFk { fk, .. } => self.open_fk_form(fk.as_deref()),
            StructA::DropFk { fk, .. } => self.stage_drop_fk(&fk),
            StructA::EditTable { .. } => self.open_table_opts(),
            StructA::Stage { changes, replaces, .. } => self.stage_changes(changes, replaces.as_deref()),
            StructA::StageTable { changes, .. } => self.stage_table_opts(changes),
            StructA::TrigNew { .. } => self.trig_new(),
            StructA::TrigRevert { key, .. } => self.trig_revert(&key),
            StructA::TrigSave { key, .. } => return self.trig_save(&key),
            StructA::TrigAskDrop { name, .. } => return self.trig_ask_drop(name),
            StructA::TrigDrop { name, .. } => return self.drop_trigger(&name),
            StructA::Maintain { what, .. } => return self.sql_maintain(what),
            StructA::RunMaint { what, .. } => {
                let sel = self.tabs[self.cur].sel.clone();
                if let Some(stmt) = self.dialect_of(sel.conn).maintenance(what, sel.space(), &sel.coll) {
                    self.sql_maintain_run(stmt);
                }
            }
        }
        Ok(Reply::Done)
    }

    /// Runs a maintenance statement of the Info page. All of them count as writes: ANALYZE and CHECK are refused by a read-only session
    /// on some servers, and the Query view's read-only gate would turn them away anyway. The heavy ones ask first.
    pub fn sql_maintain(&mut self, what: crate::db::sql::Maint) -> Result<Reply, Refusal> {
        let sel = self.tabs[self.cur].sel.clone();
        if !self.conn_writable(sel.conn) {
            return Err(self.refuse_ro(sel.conn, None));
        }
        let Some(stmt) = self.dialect_of(sel.conn).maintenance(what, sel.space(), &sel.coll) else { return Ok(Reply::Done) };
        if what.heavy() {
            self.ui_fx.push(UiFx::Open(Dialog::SqlMaintain { what, stmt }));
        } else {
            self.sql_maintain_run(stmt);
        }
        Ok(Reply::Done)
    }

    /// The Query view runs each statement on its own outside any transaction, so VACUUM works, and it logs the statement and shows the server's rows.
    pub fn sql_maintain_run(&mut self, stmt: String) {
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            s.query = format!("{stmt};");
        }
        self.tabs[self.cur].view = crate::state::View::Sql;
        self.sql_run(true, false);
    }

    /// Asks for the current tab's structure. The answer is matched back by `idx_req`.
    pub fn request_structure(&mut self) {
        let i = self.cur;
        let Some((db, space)) = self.tabs.get(i).and_then(|t| self.sql_target(&t.sel)) else { return };
        self.req += 1;
        let req = self.req;
        let t = &mut self.tabs[i];
        t.idx_req = req;
        if let Some(s) = t.sql.as_mut() {
            s.st.load = Load::Loading;
        }
        self.worker.sql_structure(req, db, space, t.sel.coll.clone());
    }

    /// A structure read answered: the tab that asked for it by `idx_req` takes it. The app's dialogs that read a structure of
    /// their own take their answers first.
    pub fn structure_loaded(&mut self, req: u64, res: db::Res<Structure>) {
        if let Some(s) = self.tabs.iter_mut().find(|t| t.idx_req == req).and_then(|t| t.sql.as_mut()) {
            s.st.finish(res);
        }
    }

    pub fn structure(&self) -> Option<&Structure> {
        self.t()?.sql.as_ref()?.st.data.as_ref()
    }


    pub fn staged(&self) -> &[Change] {
        self.t().and_then(|t| t.sql.as_ref()).map_or(&[], |s| &s.st.staged)
    }


    pub fn set_staged(&mut self, v: Vec<Change>) {
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.st.staged = v;
        }
    }


    /// The engine, its version and how its sessions read quotes and backslashes, as the statements need them.
    /// The mode is the last one the server reported; the worker checks it again on the connection that runs the
    /// statement. The offline demo has no server and uses MySQL 8.4.
    pub fn alter_env(&self) -> (Dialect, String, Mode) {
        match self.t().and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.as_ref()) {
            Some(d) => (d.dialect, d.version.clone(), Mode::of(&d.sql_mode)),
            None if self.t().is_some_and(|t| self.is_pg(t.sel.conn)) => (Dialect::Postgres, "18.0".into(), Mode::DEFAULT),
            None => (Dialect::MySql, "8.4.0".into(), Mode::DEFAULT),
        }
    }


    pub fn applying(&self) -> bool {
        self.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.st.applying)
    }


    /// The statement and its undo for these changes, as they will run. `st` is passed in because the Structure
    /// view takes the tab's structure out while it draws.
    pub fn plan_of(&self, st: &Structure, changes: &[Change]) -> Result<Plan, String> {
        let (d, v, m) = self.alter_env();
        ddl::plan(d, &v, m, &self.t().map(|t| t.sel.coll.clone()).unwrap_or_default(), st, changes)
    }


    /// The name a column has once the staged changes ran.
    pub fn name_after(&self, name: &str) -> String {
        self.staged().iter().find_map(|c| match c {
            Change::Modify { from, def, .. } if from == name => Some(def.name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| name.to_string())
    }


    pub fn stage_drop_column(&mut self, i: usize) {
        let Some(name) = self.structure().and_then(|s| s.columns.get(i)).map(|c| c.name.clone()) else { return };
        let mut staged = self.staged().to_vec();
        if staged.iter().any(|c| matches!(c, Change::Drop(n) if *n == name)) {
            return;
        }
        staged.retain(|c| !matches!(c, Change::Modify { from, .. } if *from == name));
        staged.push(Change::Drop(name));
        self.set_staged(staged);
    }


    pub fn stage_move_column(&mut self, i: usize, by: isize) {
        let Some(st) = self.structure().cloned() else { return };
        let Some(col) = st.columns.get(i) else { return };
        let mut staged = self.staged().to_vec();
        if staged.iter().any(|ch| matches!(ch, Change::Drop(n) if *n == col.name)) {
            return self.warn(format!("{} is staged to be dropped. Remove that change first.", col.name));
        }
        let order = ddl::columns_after(&st, &staged);
        let cur = self.name_after(&col.name);
        let Some(p) = order.iter().position(|n| *n == cur) else { return };
        let q = p as isize + by;
        if q < 0 || q as usize >= order.len() {
            return;
        }
        let mut rem = order;
        rem.remove(p);
        let pos = if q == 0 { Pos::First } else { Pos::After(rem[q as usize - 1].clone()) };
        let existing = staged.iter().position(|ch| matches!(ch, Change::Modify { from, .. } if *from == col.name));
        let def = match existing.map(|k| staged.remove(k)) {
            Some(Change::Modify { def, .. }) => def,
            _ => match ColDef::in_table(&st, col) {
                Ok(d) => d,
                Err(why) => return self.warn(why),
            },
        };
        staged.push(Change::Modify { from: col.name.clone(), def, pos });
        self.set_staged(staged);
    }


    /// Stages column `i` moved to where column `to` is, as the Structure grid shows them. The grid lists the table as the
    /// server has it, so the move is counted in the order the staged changes leave: the column takes `to`'s place there.
    pub fn stage_move_column_to(&mut self, i: usize, to: usize) {
        let Some(st) = self.structure().cloned() else { return };
        let (Some(a), Some(b)) = (st.columns.get(i), st.columns.get(to)) else { return };
        let order = ddl::columns_after(&st, self.staged());
        let at = |name: &str| order.iter().position(|n| *n == self.name_after(name));
        if let (Some(p), Some(q)) = (at(&a.name), at(&b.name)) {
            self.stage_move_column(i, q as isize - p as isize);
        }
    }


    pub fn stage_drop_index(&mut self, i: usize) {
        let Some(ix) = self.structure().and_then(|s| s.indexes.get(i)) else { return };
        // The server reports the expression in a form undo could not add back.
        // PostgreSQL's own text of the index brings it back exactly, so only MySQL's functional parts are refused.
        if ix.def.is_empty() && ix.cols.iter().any(|c| c.expr.is_some()) {
            return self.warn(format!("{} has an expression part, so dropping it here could not be undone. Drop it with a query.", ix.name));
        }
        let name = ix.name.clone();
        let mut staged = self.staged().to_vec();
        if staged.iter().any(|c| matches!(c, Change::DropIndex(n) if *n == name)) {
            return;
        }
        staged.retain(|c| !matches!(c, Change::RenameIndex { from, .. } if *from == name));
        staged.push(Change::DropIndex(name));
        self.set_staged(staged);
    }


    pub fn unstage(&mut self, i: usize) {
        let mut staged = self.staged().to_vec();
        if i < staged.len() {
            staged.remove(i);
            self.set_staged(staged);
        }
    }


    pub fn discard_staged(&mut self) {
        self.set_staged(Vec::new());
    }


    /// Asks for confirmation when the changes lose data or cannot be undone, then runs them.
    pub fn request_alter_apply(&mut self) -> Result<Reply, Refusal> {
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return Ok(Reply::Done);
        }
        if !self.sel_writable() {
            return Err(self.refuse_ro_quiet(self.t().map_or(0, |t| t.sel.conn)));
        }
        if self.applying() {
            return Err(self.refuse(BUSY_NOTE));
        }
        let staged = self.staged().to_vec();
        let Some(st) = self.structure().cloned() else { return Ok(Reply::Done) };
        if staged.is_empty() {
            return Ok(Reply::Done);
        }
        let plan = match self.plan_of(&st, &staged) {
            Ok(p) => p,
            Err(e) => {
                self.err(e);
                return Ok(Reply::Done);
            }
        };
        let n = staged.iter().filter(|c| matches!(c, Change::Drop(_))).count();
        if n > 0 || plan.undo.is_err() {
            let Some(sel) = self.t().map(|t| t.sel.clone()) else { return Ok(Reply::Done) };
            let table = sel.coll.clone();
            let mut title = format!("Apply {} to {table}?", plural_changes(staged.len()));
            if n > 0 {
                title.push_str(&format!(" Dropping {} deletes {} values for good.", if n == 1 { "a column" } else { "columns" }, if n == 1 { "its" } else { "their" }));
            }
            match &plan.undo {
                Ok(_) if n > 0 => title.push_str(&format!(" Undo can bring back the {}, not what it held.", if n == 1 { "column" } else { "columns" })),
                Ok(_) => {}
                Err(why) => title.push_str(&format!(" This change cannot be undone: {why}")),
            }
            self.ui_fx.push(UiFx::Open(Dialog::Typed { title, expect: table, action: Danger::ApplyAlter(sel) }));
            return Ok(Reply::Done);
        }
        self.run_alter();
        Ok(Reply::Done)
    }


    /// Sends the staged statement. The undo comes from the structure read before it runs.
    pub fn run_alter(&mut self) {
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let (conn, table) = (sel.conn, sel.coll.clone());
        let Some((db, schema)) = self.sql_target(&sel) else { return self.warn_offline() };
        let writable = self.sel_writable();
        if !writable {
            return self.warn_ro();
        }
        if self.applying() {
            return self.warn(BUSY_NOTE);
        }
        let (Some(st), staged) = (self.structure().cloned(), self.staged().to_vec()) else { return };
        let (.., m) = self.alter_env();
        let plan = match self.plan_of(&st, &staged) {
            Ok(p) => p,
            Err(e) => return self.err(e),
        };
        let sql = plan.sql();
        let Plan { undo, steps, expect, .. } = plan;
        let (undo, done) = match undo {
            Ok(mut u) => {
                let what = if staged.len() == 1 { staged[0].describe().to_lowercase() } else { plural_changes(staged.len()) };
                u.label = format!("changed the structure: {what}{}", if ddl::drops_data(&staged) { " (dropped data is not restored)" } else { "" });
                (Some(u), "Changed the structure".to_string())
            }
            Err(why) => (None, format!("Changed the structure. It cannot be undone: {why}")),
        };
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            s.st.applying = true;
        }
        match expect {
            Some(expect) => self.worker.pg_alter(conn, db, schema, table, steps, expect, undo, !writable, done),
            None => self.worker.sql_alter(conn, db, schema, table, sql, m, undo, !writable, done),
        }
    }


    /// The answer to an ALTER, a trigger change or an undo of either. Only an ALTER makes the staged changes stale; a
    /// trigger answer reloads the structure even when it failed, since a failed MySQL swap may still have changed it.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_altered(&mut self, conn: u64, database: String, schema: String, table: String, res: db::Res<String>, undo: Option<UndoAction>, sql_mode: Option<String>, alter: bool, unsure: bool) {
        let ok = res.is_ok();
        let (clear, reload) = answer_effects(ok, alter, unsure);
        // The statement was refused for a stale mode; the previews follow the new one from now on.
        self.adopt_sql_mode(conn, sql_mode);
        match res {
            Ok(s) => {
                self.toast_ok(s.clone(), undo.as_ref().map(|u| u.at));
                self.ok(s);
            }
            Err(e) => self.err(e),
        }
        if let Some(u) = undo {
            self.push_undo(u);
        }
        // A tab is the answer's table when its connection, its database and its namespace, resolved as a read of it resolves
        // them (a PostgreSQL tab may leave its schema to the sidebar's pick), are the ones the answer names. Another
        // database of the connection can hold a table of the same name.
        let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sql.is_some() && answers_tab(&self.tabs[i].sel, self.sql_target(&self.tabs[i].sel).map(|(d, ns)| (d.pg_database(), ns)), (conn, &database, &schema, &table))).collect();
        let at = if database.is_empty() { (schema.clone(), String::new()) } else { (database.clone(), schema.clone()) };
        if tabs.is_empty() {
            // No tab resolves to the answer's table after a reconnect dropped the connection's pool, or when a PostgreSQL
            // tab's schema follows a sidebar pick that changed meanwhile. The tab that sent it still needs Apply back.
            for t in self.tabs.iter_mut().filter(|t| may_answer_tab(&t.sel, (conn, &database, &schema, &table))) {
                if let Some(s) = t.sql.as_mut() {
                    s.st.applying = false;
                }
            }
        }
        for i in tabs {
            if let Some(s) = self.tabs[i].sql.as_mut() {
                s.st.applying = false;
                if !alter {
                    let saving = s.st.trig_saving.take();
                    if !ok {
                        // A refused create keeps its draft: an existing trigger of that name is not the one it would make.
                        s.st.trig_made = None;
                    } else if let Some(k) = saving {
                        s.st.trig.remove(&k);
                    }
                }
                if clear {
                    s.st.staged.clear();
                }
                if reload {
                    s.st.data = None;
                    s.st.load = Load::Idle;
                }
            }
            if reload {
                // A structure read already on its way was made before this change, and its answer would put the old structure
                // back with nothing left to ask for a new one.
                self.tabs[i].idx_req = 0;
                self.sql_reload_of(i);
            }
        }
        if ok && alter {
            self.sql_refresh_schema(conn, &at.0, &at.1);
        }
    }
}
