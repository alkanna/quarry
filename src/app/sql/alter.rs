//! Editing a SQL table's columns and indexes from the Structure view. Edits are staged on the tab and become
//! one ALTER TABLE when applied; the statement text comes from `db::sql::ddl`, which is also what the previews
//! show, so what is read is what runs. Writes stay behind the connection's unlock like every other write.

use crate::app::sql::structure::{size, Load};
use crate::app::*;
use db::sql::ddl::{self, Change, ColDef, Dv, IdxCol, IndexDef, IndexKind, Mode, Plan, Pos};
use db::sql::structure::Structure;
use db::sql::Dialect;

const TYPES: &[&str] = &["TINYINT", "SMALLINT", "MEDIUMINT", "INT", "BIGINT", "DECIMAL", "FLOAT", "DOUBLE", "BIT", "CHAR", "VARCHAR", "BINARY", "VARBINARY", "TINYTEXT", "TEXT", "MEDIUMTEXT", "LONGTEXT", "TINYBLOB", "BLOB", "MEDIUMBLOB", "LONGBLOB", "ENUM", "SET", "DATE", "TIME", "DATETIME", "TIMESTAMP", "YEAR", "JSON", "GEOMETRY", "POINT", "LINESTRING", "POLYGON"];

/// Types worth a menu entry on PostgreSQL. A type there is free text, so the list only saves typing: a length goes in the
/// text, as in numeric(10,2).
const PG_TYPES: &[&str] = &["smallint", "integer", "bigint", "numeric(10,2)", "real", "double precision", "boolean", "text", "character varying(255)", "character(1)", "uuid", "date", "time", "timestamp", "timestamp with time zone", "interval", "json", "jsonb", "bytea", "inet"];

/// Collations worth a menu entry. The charset follows from the name, so one choice sets both.
const COLLATIONS: &[&str] = &["utf8mb4_0900_ai_ci", "utf8mb4_0900_as_cs", "utf8mb4_uca1400_ai_ci", "utf8mb4_unicode_ci", "utf8mb4_general_ci", "utf8mb4_bin", "utf8mb3_general_ci", "utf8mb3_bin", "latin1_swedish_ci", "latin1_bin", "ascii_general_ci", "ascii_bin"];

#[derive(Clone, Copy, PartialEq)]
enum DMode {
    None,
    Null,
    Value,
    Expr,
}

/// The Add column and Edit column dialog.
pub(in crate::app) struct ColForm {
    /// The column's name now; None adds a new one.
    from: Option<String>,
    def: ColDef,
    /// What the form opened with, so "nothing changed" is known.
    orig: Option<(ColDef, Pos)>,
    dmode: DMode,
    dtext: String,
    pos: Pos,
    /// The staged changes this one goes after.
    rest: Vec<Change>,
    /// The other columns in the order the staged changes leave them, for the position list.
    order: Vec<String>,
    memo: Option<PlanMemo>,
}

pub(in crate::app) struct IdxForm {
    /// The index being edited; None adds one.
    from: Option<IndexDef>,
    def: IndexDef,
    /// The columns of the table with their types, in order.
    avail: Vec<(String, String)>,
    rest: Vec<Change>,
    view: Option<IdxView>,
}

/// The checks, the plan and the warnings of the index as it stands, kept until the definition changes.
struct IdxView {
    def: IndexDef,
    changes_empty: bool,
    /// The staged changes with this one added, which Stage change keeps.
    all: Vec<Change>,
    text: Result<String, String>,
    warnings: Vec<String>,
}

/// The plan and the warnings of a list of changes, kept while the changes, the table definition and the session
/// mode stay as they are. Both walk the whole structure and a bar or dialog draws them every frame.
pub(in crate::app) struct PlanMemo {
    changes: Vec<Change>,
    create: String,
    mode: Mode,
    pub(in crate::app) warnings: Vec<String>,
    pub(in crate::app) plan: Result<Plan, String>,
}

impl PlanMemo {
    pub(in crate::app) fn of<'a>(slot: &'a mut Option<PlanMemo>, app: &App, st: &Structure, changes: &[Change]) -> &'a PlanMemo {
        let mode = app.cur_mode();
        if !slot.as_ref().is_some_and(|m| m.mode == mode && m.create == st.create && m.changes == changes) {
            *slot = Some(PlanMemo { changes: changes.to_vec(), create: st.create.clone(), mode, warnings: ddl::warnings(st, changes), plan: app.plan_of(st, changes) });
        }
        slot.as_ref().unwrap()
    }
}

/// Columns the staged changes drop and columns they change, for marks beside the grid rows.
pub(in crate::app) fn staged_marks(staged: &[Change]) -> (HashSet<String>, HashSet<String>) {
    let (mut dropped, mut changed) = (HashSet::new(), HashSet::new());
    for c in staged {
        match c {
            Change::Drop(n) => {
                dropped.insert(n.clone());
            }
            Change::Modify { from, .. } => {
                changed.insert(from.clone());
            }
            _ => {}
        }
    }
    (dropped, changed)
}

fn takes_length(ty: &str) -> bool {
    !matches!(ty, "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" | "DATE" | "JSON" | "GEOMETRY" | "POINT" | "LINESTRING" | "POLYGON" | "ENUM" | "SET")
}

fn default_args(ty: &str) -> &'static str {
    match ty {
        "VARCHAR" | "VARBINARY" => "255",
        "CHAR" | "BINARY" | "BIT" => "1",
        "DECIMAL" => "10,2",
        _ => "",
    }
}

fn dflt_of(mode: DMode, text: &str) -> Dv {
    match mode {
        DMode::None => Dv::None,
        DMode::Null => Dv::Null,
        DMode::Value => Dv::Value(text.to_string()),
        DMode::Expr => Dv::Expr(text.to_string()),
    }
}

fn mode_of(d: &Dv) -> (DMode, String) {
    match d {
        Dv::None => (DMode::None, String::new()),
        Dv::Null => (DMode::Null, String::new()),
        Dv::Value(v) => (DMode::Value, v.clone()),
        Dv::Expr(v) => (DMode::Expr, v.clone()),
    }
}

/// A highlighted statement in a box, scrolling both ways.
pub(in crate::app) fn sql_block(ui: &mut egui::Ui, salt: &str, text: &str, max_h: f32) {
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt(salt).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            ui.add(egui::Label::new(sql_job(text, TXT_SMALL)).wrap());
        });
    });
}

impl App {
    pub(in crate::app) fn structure(&self) -> Option<&Structure> {
        self.t()?.sql.as_ref()?.st.data.as_ref()
    }

    pub(in crate::app) fn staged(&self) -> &[Change] {
        self.t().and_then(|t| t.sql.as_ref()).map_or(&[], |s| &s.st.staged)
    }

    pub(in crate::app) fn set_staged(&mut self, v: Vec<Change>) {
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.st.staged = v;
        }
    }

    /// The engine, its version and how its sessions read quotes and backslashes, as the statements need them.
    /// The mode is the last one the server reported; the worker checks it again on the connection that runs the
    /// statement. The offline demo has no server and uses MySQL 8.4.
    pub(in crate::app) fn alter_env(&self) -> (Dialect, String, Mode) {
        match self.t().and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.as_ref()) {
            Some(d) => (d.dialect, d.version.clone(), Mode::of(&d.sql_mode)),
            None if self.t().is_some_and(|t| self.is_pg(t.sel.conn)) => (Dialect::Postgres, "18.0".into(), Mode::DEFAULT),
            None => (Dialect::MySql, "8.4.0".into(), Mode::DEFAULT),
        }
    }

    pub(in crate::app) fn applying(&self) -> bool {
        self.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.st.applying)
    }

    /// The sql_mode the open tab's connection last reported.
    fn cur_mode(&self) -> Mode {
        self.t().map_or(Mode::DEFAULT, |t| self.sql_mode(t.sel.conn))
    }

    /// The statement and its undo for these changes, as they will run. `st` is passed in because the Structure
    /// view takes the tab's structure out while it draws.
    fn plan_of(&self, st: &Structure, changes: &[Change]) -> Result<Plan, String> {
        let (d, v, m) = self.alter_env();
        ddl::plan(d, &v, m, &self.t().map(|t| t.sel.coll.clone()).unwrap_or_default(), st, changes)
    }

    fn engine_label(&self) -> String {
        self.t().and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.as_ref()).map_or_else(|| "MySQL 8.4".into(), |d| d.label())
    }

    /// The ALTER for these changes, as it will run.
    pub(in crate::app) fn alter_text(&self, st: &Structure, changes: &[Change]) -> Result<String, String> {
        self.plan_of(st, changes).map(|p| p.sql())
    }

    /// The name a column has once the staged changes ran.
    fn name_after(&self, name: &str) -> String {
        self.staged().iter().find_map(|c| match c {
            Change::Modify { from, def, .. } if from == name => Some(def.name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| name.to_string())
    }

    pub(in crate::app) fn open_col_form(&mut self, i: Option<usize>) {
        let Some(st) = self.structure().cloned() else { return };
        let staged = self.staged().to_vec();
        let Some(i) = i else {
            let mut def = ColDef::new("", "VARCHAR");
            def.args = "255".into();
            if self.alter_env().0 == Dialect::Postgres {
                (def.ty, def.args) = ("character varying(255)".into(), String::new());
            }
            return self.show_col_form(None, def, Pos::Keep, staged);
        };
        let c = &st.columns[i];
        if staged.iter().any(|ch| matches!(ch, Change::Drop(n) if *n == c.name)) {
            return self.warn(format!("{} is staged to be dropped. Remove that change first.", c.name));
        }
        let (mine, rest): (Vec<Change>, Vec<Change>) = staged.into_iter().partition(|ch| matches!(ch, Change::Modify { from, .. } if *from == c.name));
        let (def, pos) = match mine.into_iter().next() {
            Some(Change::Modify { def, pos, .. }) => (def, pos),
            _ => match ColDef::in_table(&st, c) {
                Ok(d) => (d, Pos::Keep),
                Err(why) => return self.warn(why),
            },
        };
        self.show_col_form(Some(c.name.clone()), def, pos, rest);
    }

    fn show_col_form(&mut self, from: Option<String>, def: ColDef, pos: Pos, rest: Vec<Change>) {
        let Some(st) = self.structure() else { return };
        let order: Vec<String> = ddl::columns_after(st, &rest).into_iter().filter(|n| Some(n) != from.as_ref()).collect();
        let (dmode, dtext) = mode_of(&def.default);
        let orig = from.is_some().then(|| (def.clone(), pos.clone()));
        self.modal = Some(Modal::ColEdit(Box::new(ColForm { from, def, orig, dmode, dtext, pos, rest, order, memo: None })));
    }

    pub(in crate::app) fn duplicate_column(&mut self, i: usize) {
        let Some(st) = self.structure().cloned() else { return };
        let c = &st.columns[i];
        let mut def = match ColDef::in_table(&st, c) {
            Ok(d) => d,
            Err(why) => return self.warn(why),
        };
        let names = ddl::columns_after(&st, self.staged());
        def.name = ddl::free_name(&names, &c.name);
        // A second counter or a second copy of a unique key would not be a copy.
        def.auto_increment = false;
        // PostgreSQL has no column order, so the copy goes last.
        let pos = if self.alter_env().0 == Dialect::Postgres { Pos::Keep } else { Pos::After(self.name_after(&c.name)) };
        self.show_col_form(None, def, pos, self.staged().to_vec());
    }

    pub(in crate::app) fn stage_drop_column(&mut self, i: usize) {
        let Some(name) = self.structure().and_then(|s| s.columns.get(i)).map(|c| c.name.clone()) else { return };
        let mut staged = self.staged().to_vec();
        if staged.iter().any(|c| matches!(c, Change::Drop(n) if *n == name)) {
            return;
        }
        staged.retain(|c| !matches!(c, Change::Modify { from, .. } if *from == name));
        staged.push(Change::Drop(name));
        self.set_staged(staged);
    }

    pub(in crate::app) fn stage_move_column(&mut self, i: usize, by: isize) {
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

    pub(in crate::app) fn open_idx_form(&mut self, i: Option<usize>) {
        let Some(st) = self.structure().cloned() else { return };
        let staged = self.staged().to_vec();
        let from = i.and_then(|i| st.indexes.get(i)).map(IndexDef::from_index);
        if let Some(f) = from.as_ref().filter(|f| f.functional) {
            return self.warn(format!("{} has an expression part, which this editor cannot write back. Change it with a query.", f.name));
        }
        if let Some(f) = &from {
            if staged.iter().any(|c| matches!(c, Change::DropIndex(n) if *n == f.name) || matches!(c, Change::RenameIndex { from, .. } if *from == f.name)) {
                return self.warn(format!("{} already has a staged change. Remove it first.", f.name));
            }
        }
        let after = ddl::columns_after(&st, &staged);
        let ty = |n: &str| st.columns.iter().find(|c| c.name == n).map(|c| c.full_type.clone()).or_else(|| staged.iter().find_map(|c| match c {
            Change::Add { def, .. } | Change::Modify { def, .. } if def.name == n => Some(def.ty.to_ascii_lowercase()),
            _ => None,
        })).unwrap_or_default();
        let avail = after.iter().map(|n| (n.clone(), ty(n))).collect();
        let def = from.clone().unwrap_or_default();
        self.modal = Some(Modal::IdxEdit(Box::new(IdxForm { from, def, avail, rest: staged, view: None })));
    }

    pub(in crate::app) fn stage_drop_index(&mut self, i: usize) {
        let Some(ix) = self.structure().and_then(|s| s.indexes.get(i)) else { return };
        // The server reports the expression in a form undo could not add back.
        if ix.cols.iter().any(|c| c.expr.is_some()) {
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

    pub(in crate::app) fn unstage(&mut self, i: usize) {
        let mut staged = self.staged().to_vec();
        if i < staged.len() {
            staged.remove(i);
            self.set_staged(staged);
        }
    }

    pub(in crate::app) fn discard_staged(&mut self) {
        self.set_staged(Vec::new());
    }

    /// Asks for confirmation when the changes lose data or cannot be undone, then runs them.
    pub(in crate::app) fn request_alter_apply(&mut self) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        if !self.sel_writable() {
            return self.ask_unlock_then(|a| a.request_alter_apply());
        }
        if self.applying() {
            return self.warn(BUSY_NOTE);
        }
        let staged = self.staged().to_vec();
        let Some(st) = self.structure().cloned() else { return };
        if staged.is_empty() {
            return;
        }
        let plan = match self.plan_of(&st, &staged) {
            Ok(p) => p,
            Err(e) => return self.err(e),
        };
        let n = staged.iter().filter(|c| matches!(c, Change::Drop(_))).count();
        if n > 0 || plan.undo.is_err() {
            let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
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
            self.modal = Some(Modal::Typed { title, expect: table, typed: String::new(), action: Danger::ApplyAlter(sel) });
            return;
        }
        self.run_alter();
    }

    /// Sends the staged statement. The undo comes from the structure read before it runs.
    pub(in crate::app) fn run_alter(&mut self) {
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

    /// Before a structure undo runs: one that drops a column or converts a type back loses what was written since,
    /// so it asks for the table name like a drop does. True when it asked, and the undo waits for the answer.
    pub(in crate::app) fn alter_undo_gate(&mut self, i: usize) -> bool {
        let Some(a) = self.undo.get(i) else { return false };
        let UndoKind::Sql { undo: db::sql::write::SqlUndo::Alter(u), .. } = &a.kind else { return false };
        if u.loss.is_empty() {
            return false;
        }
        let title = format!("Undo \"{}\" on {}? {} This cannot be undone.", u.label, a.coll, u.loss.join(" "));
        self.modal = Some(Modal::Typed { title, expect: a.coll.clone(), typed: String::new(), action: Danger::UndoAlter(a.at) });
        true
    }

    /// The answer to an ALTER, a trigger change or an undo of either. Only an ALTER makes the staged changes stale; a
    /// trigger answer reloads the structure even when it failed, since a failed MySQL swap may still have changed it.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn sql_altered(&mut self, conn: u64, database: String, schema: String, table: String, res: db::Res<String>, undo: Option<UndoAction>, sql_mode: Option<String>, alter: bool, unsure: bool) {
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
        for i in tabs {
            if let Some(s) = self.tabs[i].sql.as_mut() {
                s.st.applying = false;
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
                self.reload_of(i);
            }
        }
        if ok && alter {
            self.sql_refresh_schema(conn, &at.0, &at.1);
        }
    }

    /// The staged changes as a bar above the grids: the list, what cannot be undone, and the statement.
    pub(in crate::app) fn staged_bar(&self, ui: &mut egui::Ui, memo: &mut Option<PlanMemo>, st: &Structure, staged: &[Change], editable: bool, applying: bool, acts: &mut Vec<crate::app::sql::structure::Act>) {
        use crate::app::sql::structure::Act;
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                caption(ui, "STAGED CHANGES");
                stat_chip(ui, &staged.len().to_string());
                ui.label(RichText::new("Nothing runs until Apply.").size(TXT_SMALL).color(FAINT()));
                right_group(ui, |ui| {
                    let label = if applying { "Applying..." } else { "Apply" };
                    // A locked connection is asked to unlock when Apply is pressed; only a view stops it.
                    let can = editable || !self.sel_is_view();
                    let why = if can { "" } else { "This table cannot be changed here" };
                    if ui.add_enabled(can && !applying, primary_button(ui, label, PRIMARY())).on_disabled_hover_text(why).clicked() {
                        acts.push(Act::Apply);
                    }
                    if quiet_enabled(ui, !applying, "Discard").on_hover_text("Drop every staged change").clicked() {
                        acts.push(Act::Discard);
                    }
                });
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt("staged-list").max_height(96.0).auto_shrink([false, true]).show(ui, |ui| {
                for (i, c) in staged.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(c.describe()).size(TXT_SMALL).color(TEXT()));
                        if quiet_enabled(ui, !applying, "Remove").on_hover_text("Take this change out").clicked() {
                            acts.push(Act::Unstage(i));
                        }
                    });
                }
            });
            let memo = PlanMemo::of(memo, self, st, staged);
            for w in &memo.warnings {
                ui.label(RichText::new(w.as_str()).size(TXT_SMALL).color(WARNING()));
            }
            ui.add_space(6.0);
            match &memo.plan {
                Ok(p) => {
                    if let Err(why) = &p.undo {
                        ui.label(RichText::new(format!("This change cannot be undone: {why}")).size(TXT_SMALL).color(ALERT()));
                        ui.add_space(4.0);
                    }
                    sql_block(ui, "staged-sql", &p.sql(), 140.0);
                }
                Err(e) => {
                    ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
                }
            }
        });
    }

    pub(in crate::app) fn col_dialog(&mut self, ui: &mut egui::Ui, f: &mut ColForm, keep: &mut bool) {
        let (dialect, ..) = self.alter_env();
        let pg = dialect == Dialect::Postgres;
        let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        ui.set_width(600.0);
        dialog_title(ui, if f.from.is_some() { "Edit column" } else { "Add column" });
        ui.label(RichText::new(format!("{table}.{}", f.from.as_deref().unwrap_or("new column"))).size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        let ty = f.def.ty.to_ascii_uppercase();
        egui::Grid::new("colform").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 20.0).show(ui, |ui| {
            form_label(ui, "Name");
            ui.add(egui::TextEdit::singleline(&mut f.def.name).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Column name"));
            ui.end_row();

            form_label(ui, "Type");
            if pg {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.def.ty).font(egui::TextStyle::Monospace).desired_width(280.0).hint_text("numeric(10,2)")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Column type"));
                    egui::ComboBox::from_id_salt("coltype").width(150.0).selected_text("Common types").show_ui(ui, |ui| {
                        menu_look(ui);
                        for t in PG_TYPES {
                            if ui.selectable_label(f.def.ty == *t, *t).clicked() {
                                f.def.ty = (*t).to_string();
                            }
                        }
                    });
                });
            } else {
            ui.horizontal(|ui| {
                let before = f.def.ty.clone();
                let combo = egui::ComboBox::from_id_salt("coltype").width(170.0).selected_text(f.def.ty.to_ascii_uppercase()).show_ui(ui, |ui| {
                    menu_look(ui);
                    if !TYPES.contains(&ty.as_str()) {
                        ui.selectable_value(&mut f.def.ty, ty.clone(), ty.as_str());
                    }
                    for t in TYPES {
                        ui.selectable_value(&mut f.def.ty, (*t).to_string(), *t);
                    }
                });
                combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Column type"));
                if f.def.ty != before {
                    let t = f.def.ty.to_ascii_uppercase();
                    f.def.args = default_args(&t).into();
                    if !ddl::is_numeric(&t) {
                        (f.def.unsigned, f.def.zerofill) = (false, false);
                    }
                    if !ddl::is_integer(&t) {
                        f.def.auto_increment = false;
                    }
                    if !ddl::is_text(&t) {
                        f.def.collation = None;
                    }
                }
                let t = f.def.ty.to_ascii_uppercase();
                if !matches!(t.as_str(), "ENUM" | "SET") {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Length").color(MUTED()));
                    ui.add_enabled(takes_length(&t), egui::TextEdit::singleline(&mut f.def.args).font(egui::TextStyle::Monospace).desired_width(90.0).hint_text("12,2"));
                }
            });
            ui.end_row();
            }

            let t = f.def.ty.to_ascii_uppercase();
            if !pg && matches!(t.as_str(), "ENUM" | "SET") {
                form_label(ui, "Values");
                ui.add(egui::TextEdit::singleline(&mut f.def.args).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).hint_text("'a','b','c'"));
                ui.end_row();
            }

            form_label(ui, "Options");
            ui.horizontal(|ui| {
                if pg {
                    ui.checkbox(&mut f.def.nullable, "Allow NULL");
                    ui.add_enabled(db::sql::pg::ddl::identity_type(&f.def.ty), egui::Checkbox::new(&mut f.def.auto_increment, "Identity (auto increment)"));
                    // An identity is never NULL.
                    if f.def.auto_increment {
                        f.def.nullable = false;
                    }
                } else {
                ui.add_enabled(ddl::is_numeric(&t), egui::Checkbox::new(&mut f.def.unsigned, "Unsigned"));
                ui.checkbox(&mut f.def.nullable, "Allow NULL");
                ui.add_enabled(ddl::is_numeric(&t), egui::Checkbox::new(&mut f.def.zerofill, "Zero fill"));
                ui.add_enabled(ddl::is_integer(&t), egui::Checkbox::new(&mut f.def.auto_increment, "Auto increment"));
                }
            });
            ui.end_row();

            form_label(ui, "Default");
            ui.horizontal(|ui| {
                if !f.def.nullable && f.dmode == DMode::Null {
                    f.dmode = DMode::None;
                }
                segmented(ui, &mut f.dmode, &[(DMode::None, "None"), (DMode::Null, "NULL"), (DMode::Value, "Value"), (DMode::Expr, "Expression")]);
                let on = matches!(f.dmode, DMode::Value | DMode::Expr);
                ui.add_enabled(on, egui::TextEdit::singleline(&mut f.dtext).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).hint_text(if f.dmode == DMode::Expr { if pg { "now()" } else { "CURRENT_TIMESTAMP" } } else { "" })).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, on, "Column default"));
            });
            ui.end_row();

            if !pg {
            form_label(ui, "Collation");
            let text = ddl::is_text(&t);
            let shown = if text { f.def.collation.clone().unwrap_or_else(|| "Table default".into()) } else if ddl::is_numeric(&t) { "Not used for numbers".into() } else { "Not used for this type".into() };
            ui.add_enabled_ui(text, |ui| {
                egui::ComboBox::from_id_salt("colcoll").width(ui.available_width()).selected_text(shown).show_ui(ui, |ui| {
                    menu_look(ui);
                    ui.selectable_value(&mut f.def.collation, None, "Table default");
                    let cur = f.def.collation.clone().filter(|c| !COLLATIONS.contains(&c.as_str()));
                    if let Some(c) = cur {
                        ui.selectable_value(&mut f.def.collation, Some(c.clone()), c);
                    }
                    for c in COLLATIONS.iter().filter(|c| match dialect { Dialect::MySql => !c.contains("uca1400"), Dialect::MariaDb => !c.contains("0900"), Dialect::Postgres => false }) {
                        ui.selectable_value(&mut f.def.collation, Some((*c).to_string()), *c);
                    }
                });
            });
            ui.end_row();

            }

            form_label(ui, "Comment");
            ui.add(egui::TextEdit::singleline(&mut f.def.comment).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Column comment"));
            ui.end_row();

            if !pg {
            form_label(ui, "Position");
            let keep_label = if f.from.is_some() { "Unchanged" } else { "Last" };
            let shown = match &f.pos {
                Pos::Keep => keep_label.to_string(),
                Pos::First => "First".to_string(),
                Pos::After(c) => format!("After {c}"),
            };
            egui::ComboBox::from_id_salt("colpos").width(240.0).selected_text(shown).show_ui(ui, |ui| {
                menu_look(ui);
                ui.selectable_value(&mut f.pos, Pos::Keep, keep_label);
                ui.selectable_value(&mut f.pos, Pos::First, "First");
                for n in &f.order {
                    ui.selectable_value(&mut f.pos, Pos::After(n.clone()), format!("After {n}"));
                }
            });
            ui.end_row();
            }
        });
        f.def.default = dflt_of(f.dmode, &f.dtext);
        // A position that names a column that no longer exists would break the statement.
        if let Pos::After(c) = &f.pos {
            if !f.order.contains(c) {
                f.pos = Pos::Keep;
            }
        }

        let change = match &f.from {
            None => Change::Add { def: f.def.clone(), pos: f.pos.clone() },
            Some(from) => Change::Modify { from: from.clone(), def: f.def.clone(), pos: f.pos.clone() },
        };
        let mut all = f.rest.clone();
        all.push(change);
        let unchanged = f.orig.as_ref().is_some_and(|(d, p)| *d == f.def && *p == f.pos);
        let dup = f.order.iter().any(|n| ddl::same_name(n, &f.def.name));
        let st = self.structure();
        let memo = st.map(|st| PlanMemo::of(&mut f.memo, self, st, &all));
        let plan: Result<&Plan, String> = if dup {
            Err(format!("The table already has a column called {}.", f.def.name))
        } else if unchanged {
            Err("Nothing is changed yet.".into())
        } else {
            memo.map_or_else(|| Err("The structure is not loaded.".into()), |m| m.plan.as_ref().map_err(Clone::clone))
        };
        let text = plan.as_ref().map(|p| p.sql()).map_err(Clone::clone);
        ui.add_space(6.0);
        caption(ui, "SQL TO RUN");
        ui.add_space(4.0);
        match &text {
            Ok(t) => sql_block(ui, "col-sql", t, 130.0),
            Err(e) => {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(if unchanged { FAINT() } else { ALERT() }));
            }
        }
        if let Ok(Plan { undo: Err(why), .. }) = &plan {
            ui.label(RichText::new(format!("This change cannot be undone: {why}")).size(TXT_SMALL).color(ALERT()));
        }
        if let (Some(st), Some(memo)) = (st, memo) {
            for w in &memo.warnings {
                ui.label(RichText::new(w.as_str()).size(TXT_SMALL).color(WARNING()));
            }
            ui.add_space(4.0);
            let sz = st.status.data.zip(st.status.index).map(|(a, b)| size(a + b)).unwrap_or_default();
            let note = if pg {
                format!("Runs in one transaction that locks the table{} against every other session until it commits, and rolls back whole if anything fails.", if sz.is_empty() { String::new() } else { format!(" ({sz})") })
            } else {
                format!("The server may copy the table{} on {}. Other sessions can still read.", if sz.is_empty() { String::new() } else { format!(" ({sz})") }, self.engine_label())
            };
            ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
        }
        let (mut stage, mut now) = (false, false);
        dialog_actions(ui, |ui| {
            stage = ui.add_enabled(text.is_ok(), primary_button(ui, "Stage change", PRIMARY())).clicked();
            let busy = self.applying();
            now = ui.add_enabled(text.is_ok() && !self.sel_is_view() && !busy, egui::Button::new("Apply now")).on_hover_text("Stage this change and run everything staged. A locked connection asks to unlock first.").on_disabled_hover_text(if busy { BUSY_NOTE } else { "A view cannot be changed here" }).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if stage || now {
            *keep = false;
            self.set_staged(all);
            if now {
                self.request_alter_apply();
            }
        }
    }

    /// What the index dialog shows for its current definition: the changes it stages, the SQL or the reason there is none, and the warnings.
    fn idx_view(&self, f: &IdxForm) -> IdxView {
        let changes: Vec<Change> = match &f.from {
            None => vec![Change::AddIndex(f.def.clone())],
            Some(old) if *old == f.def => Vec::new(),
            Some(old) if IndexDef { name: old.name.clone(), ..f.def.clone() } == *old && f.def.kind != IndexKind::Primary => vec![Change::RenameIndex { from: old.name.clone(), to: f.def.name.clone(), def: old.clone() }],
            Some(old) => vec![Change::DropIndex(old.name.clone()), Change::AddIndex(f.def.clone())],
        };
        let kinds: Vec<(String, String)> = f.def.cols.iter().map(|c| (c.name.clone(), f.avail.iter().find(|a| a.0 == c.name).map(|a| a.1.clone()).unwrap_or_default())).collect();
        let mut all = f.rest.clone();
        all.extend(changes.clone());
        // Against the indexes the other staged changes leave, without case, as the server compares them.
        let taken = self.structure().map(|s| ddl::indexes_after(s, &f.rest)).unwrap_or_default();
        let dup = f.def.kind != IndexKind::Primary && taken.iter().any(|x| ddl::same_name(x, &f.def.name) && f.from.as_ref().is_none_or(|o| !ddl::same_name(&o.name, x)));
        let primary_taken = f.def.kind == IndexKind::Primary && f.from.as_ref().map(|o| o.kind) != Some(IndexKind::Primary) && self.structure().is_some_and(|s| s.indexes.iter().any(|x| x.name == "PRIMARY" || x.primary)) && !all.iter().any(|c| matches!(c, Change::DropIndex(n) if n == "PRIMARY" || self.structure().is_some_and(|s| s.indexes.iter().any(|x| x.primary && x.name == *n))));
        let text = if changes.is_empty() {
            Err("Nothing is changed yet.".to_string())
        } else if dup {
            Err(format!("The table already has an index called {}.", f.def.name))
        } else if primary_taken {
            Err("The table already has a primary key. Drop it first.".to_string())
        } else if let Some(p) = ddl::index_problem(f.def.kind, &kinds) {
            Err(p)
        } else {
            self.structure().map_or_else(|| Err("The structure is not loaded.".into()), |st| self.alter_text(st, &all))
        };
        let warnings = self.structure().map(|st| ddl::warnings(st, &all)).unwrap_or_default();
        IdxView { def: f.def.clone(), changes_empty: changes.is_empty(), all, text, warnings }
    }

    pub(in crate::app) fn idx_dialog(&mut self, ui: &mut egui::Ui, f: &mut IdxForm, keep: &mut bool) {
        let pg = self.alter_env().0 == Dialect::Postgres;
        let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        ui.set_width(560.0);
        dialog_title(ui, if f.from.is_some() { "Edit index" } else { "Add index" });
        ui.label(RichText::new(format!("{table}.{}", f.from.as_ref().map_or("new index", |d| d.name.as_str()))).size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        egui::Grid::new("idxform").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Name");
            let primary = f.def.kind == IndexKind::Primary;
            // A PostgreSQL primary key is a named constraint, and its usual name is the table's with _pkey.
            let pkey = format!("{table}_pkey");
            if primary {
                if !pg {
                    f.def.name = "PRIMARY".into();
                } else if f.def.name.is_empty() || f.def.name == "PRIMARY" {
                    f.def.name = pkey.clone();
                }
            } else if f.def.name == "PRIMARY" || (pg && f.def.name == pkey && f.from.as_ref().is_none_or(|o| o.name != pkey)) {
                f.def.name.clear();
            }
            if f.def.kind == IndexKind::Index {
                f.def.constraint = false;
            }
            ui.add_enabled(!primary || pg, egui::TextEdit::singleline(&mut f.def.name).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, !primary || pg, "Index name"));
            ui.end_row();

            form_label(ui, "Type");
            if pg {
                segmented(ui, &mut f.def.kind, &[(IndexKind::Index, "Index"), (IndexKind::Unique, "Unique"), (IndexKind::Primary, "Primary key")]);
            } else {
                segmented(ui, &mut f.def.kind, &[(IndexKind::Index, "Index"), (IndexKind::Unique, "Unique"), (IndexKind::Fulltext, "Fulltext"), (IndexKind::Spatial, "Spatial"), (IndexKind::Primary, "Primary key")]);
            }
            ui.end_row();

            form_label(ui, "Columns");
            ui.vertical(|ui| {
                let mut act: Option<(usize, i8)> = None;
                for (i, c) in f.def.cols.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized([180.0, BUTTON_H], egui::Label::new(RichText::new(c.name.as_str()).monospace().color(TEXT())).truncate());
                        if !pg {
                            let mut p = c.prefix.map(|n| n.to_string()).unwrap_or_default();
                            ui.add(egui::TextEdit::singleline(&mut p).desired_width(54.0).hint_text("length"));
                            c.prefix = p.trim().parse().ok().filter(|n: &u64| *n > 0);
                        }
                        ui.checkbox(&mut c.desc, "DESC");
                        if quiet_enabled(ui, i > 0, "Up").clicked() {
                            act = Some((i, -1));
                        }
                        if quiet(ui, "Down").clicked() {
                            act = Some((i, 1));
                        }
                        if quiet(ui, "Remove").clicked() {
                            act = Some((i, 0));
                        }
                    });
                }
                if let Some((i, by)) = act {
                    match by {
                        0 => {
                            f.def.cols.remove(i);
                        }
                        b if (i as i8 + b) >= 0 && ((i as i8 + b) as usize) < f.def.cols.len() => f.def.cols.swap(i, (i as i8 + b) as usize),
                        _ => {}
                    }
                }
                let free: Vec<&(String, String)> = f.avail.iter().filter(|(n, _)| !f.def.cols.iter().any(|c| c.name == *n)).collect();
                let mut add = None;
                let combo = egui::ComboBox::from_id_salt("idxadd").width(260.0).selected_text(if free.is_empty() { "Every column is used" } else { "Add a column" }).show_ui(ui, |ui| {
                    menu_look(ui);
                    for (n, ty) in &free {
                        if ui.selectable_label(false, format!("{n}  {}", ty.to_ascii_uppercase())).clicked() {
                            add = Some(n.clone());
                        }
                    }
                });
                combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Add a column"));
                if let Some(n) = add {
                    if f.def.name.is_empty() && f.def.cols.is_empty() {
                        f.def.name = format!("idx_{n}");
                    }
                    f.def.cols.push(IdxCol { name: n, prefix: None, desc: false });
                }
            });
            ui.end_row();

            form_label(ui, "Comment");
            ui.add(egui::TextEdit::singleline(&mut f.def.comment).desired_width(f32::INFINITY));
            ui.end_row();
        });

        // The checks and the plan walk the whole structure, so they run when the index changes and not on every frame.
        if f.view.as_ref().is_none_or(|v| v.def != f.def) {
            f.view = Some(self.idx_view(f));
        }
        let v = f.view.as_ref().unwrap();
        let (text, changes_empty) = (&v.text, v.changes_empty);
        ui.add_space(6.0);
        caption(ui, "SQL TO RUN");
        ui.add_space(4.0);
        match text {
            Ok(t) => sql_block(ui, "idx-sql", t, 130.0),
            Err(e) => {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(if changes_empty { FAINT() } else { ALERT() }));
            }
        }
        for w in &v.warnings {
            ui.label(RichText::new(w.as_str()).size(TXT_SMALL).color(WARNING()));
        }
        let text_ok = text.is_ok();
        let (mut stage, mut now) = (false, false);
        dialog_actions(ui, |ui| {
            stage = ui.add_enabled(text_ok, primary_button(ui, "Stage change", PRIMARY())).clicked();
            let busy = self.applying();
            now = ui.add_enabled(text_ok && !self.sel_is_view() && !busy, egui::Button::new("Apply now")).on_hover_text("Stage this change and run everything staged. A locked connection asks to unlock first.").on_disabled_hover_text(if busy { BUSY_NOTE } else { "A view cannot be changed here" }).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if stage || now {
            *keep = false;
            let all = f.view.take().map(|v| v.all).unwrap_or_default();
            self.set_staged(all);
            if now {
                self.request_alter_apply();
            }
        }
    }

    /// QUARRY_TEST_SQL=coledit: the dialog open on the total column of the design, and `stagebar` for the bar
    /// over the grids with a few staged changes. Offline, so the answer to Apply is the demo notice.
    pub(in crate::app) fn sql_alter_demo(&mut self, which: &str) {
        let (Some(i), true) = (self.structure().and_then(|s| s.columns.iter().position(|c| c.name == "total")), matches!(which, "coledit" | "stagebar")) else { return };
        if which == "coledit" {
            self.open_col_form(Some(i));
            if let Some(Modal::ColEdit(f)) = self.modal.as_mut() {
                f.def.args = "12,2".into();
                f.pos = Pos::After("customer_id".into());
            }
            return;
        }
        let mut def = ColDef::new("sku", "VARCHAR");
        def.args = "40".into();
        def.nullable = false;
        def.default = Dv::None;
        let staged = vec![Change::Add { def, pos: Pos::After("id".into()) }, Change::Drop("note".into()), Change::DropIndex("idx_status".into())];
        self.set_staged(staged);
    }
}

/// What an answer does to the tabs of its table: (clear the staged changes, reload the structure). A done ALTER
/// leaves the staged changes written against a structure that is gone. A trigger answer leaves them as they are,
/// and reloads even after a failure, which on MySQL may have come after the old trigger was dropped and put back.
/// Whether the answer of a structure change, as (connection, PostgreSQL database or empty, namespace, table), is about the
/// table `sel` points at. `target` is what a read of the tab resolves to: the database of its pool and its namespace, which
/// is not `sel.schema` when a PostgreSQL tab leaves its schema to the one the sidebar shows.
pub(in crate::app) fn answers_tab(sel: &Sel, target: Option<(String, String)>, answer: (u64, &str, &str, &str)) -> bool {
    let (conn, database, schema, table) = answer;
    sel.conn == conn && sel.coll == table && target.is_some_and(|(d, ns)| ns == schema && d == database)
}

/// What an answer does to the open tabs of its table: (clear the staged changes, reload the structure). An answer that does
/// not know the outcome (a COMMIT that failed) clears and reloads like a success, since the staged changes may already be
/// applied and what is on screen may be stale.
fn answer_effects(ok: bool, alter: bool, unsure: bool) -> (bool, bool) {
    ((ok || unsure) && alter, ok || !alter || unsure)
}

fn plural_changes(n: usize) -> String {
    format!("{n} structure change{}", if n == 1 { "" } else { "s" })
}

/// Shown under a disabled Apply now button.
pub(in crate::app) const BUSY_NOTE: &str = "A structure change is still running.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_belongs_to_the_tab_of_its_database_and_resolved_schema() {
        let sel = |db: &str, schema: &str| Sel { conn: 1, db: db.into(), schema: schema.into(), coll: "t".into() };
        let ans = (1, "shop", "public", "t");
        assert!(answers_tab(&sel("shop", "public"), Some(("shop".into(), "public".into())), ans));
        // a tab that leaves its schema to the sidebar's pick still matches the schema the answer names
        assert!(answers_tab(&sel("shop", ""), Some(("shop".into(), "public".into())), ans));
        // the same schema and table in another database of the connection is another table
        assert!(!answers_tab(&sel("other", "public"), Some(("other".into(), "public".into())), ans));
        assert!(!answers_tab(&sel("shop", ""), Some(("shop".into(), "audit".into())), ans));
        assert!(!answers_tab(&sel("shop", "public"), None, ans));
        // MySQL has no database name beside the namespace
        assert!(answers_tab(&sel("shop", ""), Some((String::new(), "shop".into())), (1, "", "shop", "t")));
    }

    /// Review finding 8: saving or dropping a trigger cleared the staged structure changes.
    #[test]
    fn only_an_alter_clears_the_staged_changes() {
        assert_eq!(answer_effects(true, true, false), (true, true));
        assert_eq!(answer_effects(false, true, false), (false, false));
        assert_eq!(answer_effects(true, false, false), (false, true));
        assert_eq!(answer_effects(false, false, false), (false, true), "a failed trigger change still reloads");
        assert_eq!(answer_effects(false, true, true), (true, true), "a commit that may have run reloads the table");
    }
}
