//! The dialogs that stage a change of a SQL table's structure: column, index, foreign key, CHECK and table options. Core
//! builds each form from the structure and the staged list and hands it to the app, which holds it in its Modal and draws
//! it (src/app/sql/alter.rs, rel.rs, checks.rs, table_opts.rs); the form's Stage comes back as `StructA::Stage`.

use crate::action::{Dialog, UiFx};
use crate::core::Core;
use crate::db::sql::ddl::{self, Change, ColDef, IndexDef, Pos, TableOpt};
use crate::db::sql::structure::CheckConstraint;
use crate::db::sql::Dialect;
use crate::state::sql::alter::*;
use crate::state::sql::checks::CheckForm;
use crate::state::sql::rel::FkForm;
use crate::state::sql::table_opts::TableForm;

impl Core {
    /// The column dialog on column `name`, or on a new column.
    pub fn open_col_form(&mut self, name: Option<&str>) {
        let Some(st) = self.structure().cloned() else { return };
        let staged = self.staged().to_vec();
        let Some(name) = name else {
            let mut def = ColDef::new("", "VARCHAR");
            def.args = "255".into();
            if self.alter_env().0 == Dialect::Postgres {
                (def.ty, def.args) = ("character varying(255)".into(), String::new());
            }
            return self.show_col_form(None, def, Pos::Keep, staged);
        };
        let Some(c) = st.columns.iter().find(|c| c.name == name) else { return };
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
        let (dmode, dtext) = match &def.generated {
            Some(g) => (DMode::Generated, g.expr.clone()),
            None => mode_of(&def.default),
        };
        let stored = def.generated.as_ref().is_none_or(|g| g.stored);
        let orig = from.is_some().then(|| (def.clone(), pos.clone()));
        self.ui_fx.push(UiFx::Open(Dialog::ColEdit(Box::new(ColForm { from, def, orig, dmode, dtext, stored, pos, rest, order, memo: None }))));
    }

    pub fn duplicate_column(&mut self, name: &str) {
        let Some(st) = self.structure().cloned() else { return };
        let Some(c) = st.columns.iter().find(|c| c.name == name) else { return };
        let mut def = match ColDef::in_table(&st, c) {
            Ok(d) => d,
            Err(why) => return self.warn(why),
        };
        let names = ddl::columns_after(&st, self.staged());
        def.name = ddl::free_name(&names, &c.name);
        // A second counter or a second copy of a unique key would not be a copy.
        def.auto_increment = false;
        // A MariaDB column CHECK names the column it was read from.
        def.check = None;
        // PostgreSQL has no column order, so the copy goes last.
        let pos = if self.alter_env().0 == Dialect::Postgres { Pos::Keep } else { Pos::After(self.name_after(&c.name)) };
        self.show_col_form(None, def, pos, self.staged().to_vec());
    }

    /// The index dialog on index `name`, or on a new index.
    pub fn open_idx_form(&mut self, name: Option<&str>) {
        let Some(st) = self.structure().cloned() else { return };
        let staged = self.staged().to_vec();
        let from = name.and_then(|n| st.indexes.iter().find(|x| x.name == n)).map(IndexDef::from_index);
        if let Some(f) = from.as_ref().filter(|f| f.functional) {
            let why = if self.alter_env().0 == Dialect::Postgres { "an expression, operator class, collation, NULLS order, storage option or tablespace" } else { "an expression part" };
            return self.warn(format!("{} has {why}, which this editor cannot write back. Change it with a query. Dropping it can still be undone.", f.name));
        }
        if let Some(f) = &from && staged.iter().any(|c| matches!(c, Change::DropIndex(n) if *n == f.name) || matches!(c, Change::RenameIndex { from, .. } if *from == f.name)) {
            return self.warn(format!("{} already has a staged change. Remove it first.", f.name));
        }
        let after = ddl::columns_after(&st, &staged);
        let ty = |n: &str| st.columns.iter().find(|c| c.name == n).map(|c| c.full_type.clone()).or_else(|| staged.iter().find_map(|c| match c {
            Change::Add { def, .. } | Change::Modify { def, .. } if def.name == n => Some(def.ty.to_ascii_lowercase()),
            _ => None,
        })).unwrap_or_default();
        let avail = after.iter().map(|n| (n.clone(), ty(n))).collect();
        let def = from.clone().unwrap_or_default();
        self.ui_fx.push(UiFx::Open(Dialog::IdxEdit(Box::new(IdxForm { include_text: def.include.join(", "), from, def, avail, rest: staged, view: None }))));
    }

    /// The CHECK dialog on constraint `name`, or on a new one.
    pub fn open_check_form(&mut self, name: Option<&str>) {
        let Some(st) = self.structure().cloned() else { return };
        let staged = self.staged().to_vec();
        let from = name.and_then(|n| st.checks.iter().find(|k| k.name == n)).cloned();
        if let Some(f) = &from && staged.iter().any(|c| matches!(c, Change::DropCheck(n) if *n == f.name)) {
            return self.warn(format!("{} already has a staged change. Remove it first.", f.name));
        }
        let def = match &from {
            Some(f) => f.clone(),
            None => {
                // A name to start from, so the statement reads at once and the person only has to type the condition.
                let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                let taken = ddl::checks_after(&st, &staged);
                let name = (0..).map(|n| if n == 0 { format!("chk_{table}") } else { format!("chk_{table}_{n}") }).find(|c| !taken.iter().any(|t| ddl::same_name(t, c))).unwrap_or_default();
                CheckConstraint { name, ..Default::default() }
            }
        };
        self.ui_fx.push(UiFx::Open(Dialog::CheckEdit(Box::new(CheckForm { from, def, rest: staged, view: None }))));
    }

    pub fn stage_drop_check(&mut self, name: &str) {
        let Some(k) = self.structure().and_then(|s| s.checks.iter().find(|k| k.name == name)) else { return };
        let name = k.name.clone();
        let mut staged = self.staged().to_vec();
        if staged.iter().any(|c| matches!(c, Change::DropCheck(n) if *n == name)) {
            return;
        }
        staged.push(Change::DropCheck(name));
        self.set_staged(staged);
    }

    /// The Add relation dialog, or with `edit` the dialog on that key of the table (PostgreSQL).
    pub fn open_fk_form(&mut self, edit: Option<&str>) {
        let Some(own) = self.structure().cloned() else { return };
        let from = edit.and_then(|n| own.foreign_keys.iter().find(|k| k.name == n).cloned());
        if let Some(k) = &from {
            if !k.opaque.is_empty() {
                return self.warn(format!("{} cannot be edited here: {}.", k.name, k.opaque));
            }
            let pg = self.t().is_some_and(|t| self.is_pg(t.sel.conn));
            if self.staged().iter().any(|c| matches!(c, Change::DropFk(n) | Change::RenameFk { from: n, .. } | Change::SetFkDeferral { name: n, .. } if ddl::fk_name_is(pg, n, &k.name))) {
                return self.warn(format!("{} already has a staged change. Remove it first.", k.name));
            }
        }
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        // The namespace the key points into: the database on MySQL, a schema of this database on PostgreSQL.
        let pg = self.is_pg(sel.conn);
        let ns = if pg { self.sel_schema(&sel) } else { sel.db.clone() };
        let rest = self.staged().to_vec();
        let first = own.columns.first().map(|c| c.name.clone()).unwrap_or_default();
        let (d, version, _) = self.alter_env();
        // Until the server answers, a MySQL that has the rule on by default is taken to have it.
        let strict = ddl::fk_whole_key_default(d, &version);
        let mut rules_req = 0;
        if let Some(sql) = self.t().and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.clone()).filter(|_| !pg) {
            self.req += 1;
            rules_req = self.req;
            self.worker.sql_fk_rules(rules_req, sql);
        }
        let (name, pairs) = match &from {
            Some(k) => (k.name.clone(), k.cols.iter().cloned().zip(k.ref_cols.iter().cloned()).collect()),
            None => (String::new(), vec![(first, String::new())]),
        };
        let (ref_db, ref_table) = from.as_ref().map(|k| (k.ref_db.clone(), k.ref_table.clone())).unwrap_or((ns, String::new()));
        let (on_update, on_delete) = from.as_ref().map(|k| (k.on_update.clone(), k.on_delete.clone())).unwrap_or_else(|| if pg { ("NO ACTION".into(), "NO ACTION".into()) } else { ("CASCADE".into(), "RESTRICT".into()) });
        self.ui_fx.push(UiFx::Open(Dialog::FkEdit(Box::new(FkForm {
            name,
            named: from.is_some(),
            pairs,
            ref_db,
            ref_table,
            on_update,
            on_delete,
            own_cols: own.columns.iter().map(|c| c.name.clone()).filter(|n| !rest.iter().any(|c| matches!(c, Change::Drop(d) if d == n))).collect(),
            own,
            rest,
            memo: None,
            parent: None,
            asked: (String::new(), String::new()),
            parent_req: 0,
            parent_err: None,
            orphans: None,
            orphan_for: None,
            orphan_req: 0,
            strict,
            rules_req,
            check: false,
            sample: Vec::new(),
            pg,
            match_full: from.as_ref().is_some_and(|k| k.match_full),
            deferrable: from.as_ref().is_some_and(|k| k.deferrable),
            deferred: from.as_ref().is_some_and(|k| k.deferred),
            not_valid: from.as_ref().is_some_and(|k| k.not_valid),
            from,
        }))));
    }

    pub fn stage_drop_fk(&mut self, name: &str) {
        let Some(name) = self.structure().and_then(|s| s.foreign_keys.iter().find(|k| k.name == name)).map(|k| k.name.clone()) else { return };
        let mut staged = self.staged().to_vec();
        let pg = self.t().is_some_and(|t| self.is_pg(t.sel.conn));
        if staged.iter().any(|c| matches!(c, Change::DropFk(n) if ddl::fk_name_is(pg, n, &name))) {
            return;
        }
        staged.push(Change::DropFk(name));
        self.set_staged(staged);
    }

    pub fn open_table_opts(&mut self) {
        let Some(was) = self.structure().map(|st| (st.status.comment.clone(), st.status.engine.clone(), st.status.collation.clone(), st.status.auto_increment)) else { return };
        let Some((conn, schema)) = self.t().map(|t| (t.sel.conn, t.sel.db.clone())) else { return };
        let pg = self.alter_env().0 == Dialect::Postgres;
        // The engines and collations the dialog lists; the offline demo has no server and fills them itself.
        if !pg {
            self.ask_meta(conn, &schema);
        }
        let mut auto = was.3.map(|n| n.to_string()).unwrap_or_default();
        let (mut comment, mut engine, mut collation) = (was.0.clone(), was.1.clone(), was.2.clone());
        // Start from what is staged, not from what the server holds, or a second visit would show the old values and stage them again.
        let (mut rest, mut had_staged) = (Vec::new(), false);
        for c in self.staged() {
            match c {
                Change::Table { opt, .. } => {
                    had_staged = true;
                    match opt {
                        TableOpt::Comment(v) => comment = v.clone(),
                        TableOpt::Engine(v) => engine = v.clone(),
                        TableOpt::Collation(v) => collation = v.clone(),
                        TableOpt::AutoIncrement(n) => auto = n.to_string(),
                    }
                }
                c => rest.push(c.clone()),
            }
        }
        self.ui_fx.push(UiFx::Open(Dialog::TableOpts(Box::new(TableForm { conn, schema, pg, comment, engine, collation, auto, was, rest, had_staged, meta: None, load_err: None, view: None }))));
    }

    /// A dialog's Stage: its own changes go after the list as it is now. `replaces` is the column whose staged edit it took over.
    pub fn stage_changes(&mut self, changes: Vec<Change>, replaces: Option<&str>) {
        let staged = restage(self.staged(), replaces, changes, 0);
        self.set_staged(staged);
    }

    /// The table options dialog's Stage: its changes replace every staged table option.
    pub fn stage_table_opts(&mut self, changes: Vec<Change>) {
        // From the list as it is now (an Apply that finished meanwhile emptied it), with this dialog's options replacing the staged ones.
        let mut staged: Vec<Change> = self.staged().iter().filter(|c| !matches!(c, Change::Table { .. })).cloned().collect();
        staged.extend(changes);
        self.set_staged(staged);
    }
}
