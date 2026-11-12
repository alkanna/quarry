//! Foreign keys of a SQL table: the Add relation dialog, dropping a key, and opening the row a key cell
//! points to from Content. Changes are staged like column and index edits (`sql_alter.rs`), so the statement
//! the preview shows is the one that runs and the inverse comes from the structure read before.

use crate::app::sql::alter::{sql_block, BUSY_NOTE};
use crate::app::*;
use db::sql::ddl::{self, Change, FK_ACTIONS};
use db::sql::structure::{ForeignKey, Structure};

/// Above this many rows the check for orphans waits for a click, because it joins the whole table.
const CHECK_ROWS: u64 = 200_000;

/// The height kept for the line or box that reports the rows with no match, which is a one line note, a box of a few lines or
/// nothing as the answer comes in. It is kept whatever is shown so the buttons below never move when the answer lands, where a
/// click aimed at one could hit the space it just left.
const ORPHAN_BOX_H: f32 = 64.0;

/// What the orphan count depends on, as `ddl::orphan_key` names it.
#[derive(Clone, PartialEq)]
struct OrphanKey {
    cols: Vec<String>,
    ref_db: String,
    ref_table: String,
    ref_cols: Vec<String>,
}

impl OrphanKey {
    fn of(fk: &ForeignKey) -> Self {
        let (cols, ref_db, ref_table, ref_cols) = ddl::orphan_key(fk);
        Self { cols, ref_db, ref_table, ref_cols }
    }
}

/// The Add relation dialog.
pub(in crate::app) struct FkForm {
    name: String,
    /// The name was typed, so it stops following the columns.
    named: bool,
    /// Column of this table and column it points to, in key order. Empty means not picked yet.
    pairs: Vec<(String, String)>,
    ref_db: String,
    ref_table: String,
    on_update: String,
    on_delete: String,
    own: Structure,
    /// The columns of `own` the staged changes leave, for the pickers. Neither input changes while the dialog is open.
    own_cols: Vec<String>,
    rest: Vec<Change>,
    /// The plan of the key as it stands, kept while the changes and the table stay as they are.
    memo: Option<PlanMemo>,
    parent: Option<Structure>,
    /// The schema and table `parent` was asked for, and the request that carries the answer.
    asked: (String, String),
    parent_req: u64,
    parent_err: Option<String>,
    /// How many rows point at nothing for the key as it stands, and which columns and request that is for.
    orphans: Option<Result<u64, String>>,
    orphan_for: Option<OrphanKey>,
    orphan_req: u64,
    /// The server takes a key only on a whole primary or unique key, and the request that asks it.
    strict: bool,
    rules_req: u64,
    /// The person asked for the check on a big table.
    check: bool,
    /// A few of the rows the count found, as their key values.
    sample: Vec<String>,
    /// PostgreSQL: the namespace is a schema of the tab's database, and these are the options only it has.
    pg: bool,
    match_full: bool,
    deferrable: bool,
    deferred: bool,
    not_valid: bool,
    /// The key being edited, which a change replaces (PostgreSQL). None for a new one.
    from: Option<ForeignKey>,
}

impl FkForm {
    pub(in crate::app) fn parent_loaded(&mut self, req: u64, res: db::Res<Structure>) -> bool {
        if req != self.parent_req || req == 0 {
            return false;
        }
        match res {
            Ok(s) => (self.parent, self.parent_err) = (Some(s), None),
            Err(e) => (self.parent, self.parent_err) = (None, Some(e)),
        }
        true
    }

    pub(in crate::app) fn rules_done(&mut self, req: u64, strict: bool) {
        if req == self.rules_req && req != 0 {
            self.strict = strict;
        }
    }

    pub(in crate::app) fn orphans_done(&mut self, req: u64, res: db::Res<u64>, sample: Vec<String>) -> bool {
        if req != self.orphan_req || req == 0 {
            return false;
        }
        (self.orphans, self.sample) = (Some(res), sample);
        true
    }

    /// Puts the form back in the state where the count of the rows with no match is asked and not answered yet.
    #[cfg(test)]
    pub(in crate::app) fn forget_orphan_answer(&mut self) {
        self.orphans = None;
    }

    /// The key the form describes once every part is picked.
    fn key(&self, schema: &str) -> Option<ForeignKey> {
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

/// What changing key `old` into `new` stages: a rename when only the name differs, else the key dropped and added again,
/// which PostgreSQL does in the one transaction.
fn edit_changes(old: &ForeignKey, new: &ForeignKey) -> Vec<Change> {
    let same = old.cols == new.cols && old.ref_db == new.ref_db && old.ref_table == new.ref_table && old.ref_cols == new.ref_cols && old.on_update.eq_ignore_ascii_case(&new.on_update) && old.on_delete.eq_ignore_ascii_case(&new.on_delete)
        && (old.match_full, old.deferrable, old.deferred, old.not_valid) == (new.match_full, new.deferrable, new.deferred, new.not_valid);
    match (same, old.name == new.name) {
        (true, true) => Vec::new(),
        (true, false) => vec![Change::RenameFk { from: old.name.clone(), to: new.name.clone() }],
        _ => vec![Change::DropFk(old.name.clone()), Change::AddFk(new.clone())],
    }
}

/// The actions the dialog offers, in the order of the buttons. PostgreSQL also has SET DEFAULT.
fn actions(pg: bool) -> &'static [&'static str] {
    const PG: [&str; 5] = ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION", "SET DEFAULT"];
    if pg { &PG } else { &FK_ACTIONS }
}

fn action_index(a: &str, pg: bool) -> usize {
    actions(pg).iter().position(|x| x.eq_ignore_ascii_case(a)).unwrap_or(0)
}

fn action_label(i: usize) -> &'static str {
    ["Restrict", "Cascade", "Set null", "No action", "Set default"][i]
}

fn type_text(st: &Structure, col: &str) -> String {
    st.columns.iter().find(|c| c.name.eq_ignore_ascii_case(col)).map(|c| c.full_type.to_ascii_uppercase()).unwrap_or_default()
}

/// Whether a tab holds a filter, sort or bar clause of the person's, which opening a row there would replace.
fn has_clauses(t: &Tab) -> bool {
    let bar = t.sql.as_ref().is_some_and(|s| {
        let c = &s.bar.c;
        !(c.select.is_empty() && c.group.is_empty() && c.having.is_empty() && c.limit.is_empty() && c.offset.is_empty() && c.hint.is_empty() && !c.distinct)
    });
    !t.filter_txt.trim().is_empty() || !t.sort_txt.trim().is_empty() || bar
}

/// The filter that finds the row a key points to, from the key columns of the clicked row. FLOAT and DOUBLE keys get
/// none: the grid holds their value rounded to what it shows, and `=` against that may find no row or another one.
fn fk_row_filter(d: db::sql::Dialect, bs: bool, fk: &ForeignKey, cols: &[db::sql::Col], row: &Document) -> Result<String, String> {
    let mut lits = Vec::new();
    for c in &fk.cols {
        // The row is keyed by the shown name, which differs from the server's for a renamed column.
        let col = cols.iter().find(|k| k.src == *c);
        if col.is_some_and(|k| k.kind == db::sql::value::Kind::Float) {
            return Err(format!("{c} is a floating point column, whose value cannot be matched exactly. Filter {} by hand.", fk.ref_table));
        }
        let key = col.map_or(c.as_str(), |k| k.name.as_str());
        match db::get_path(row, key) {
            Some(Bson::Null) | None => return Err(format!("{c} is NULL in this row, so it points to nothing.")),
            v => lits.push(db::copy::sql_literal(v, col.is_some_and(|k| is_numeric(k.kind)), bs)),
        }
    }
    Ok(fk.row_where(d, &lits))
}

impl App {
    /// The Add relation dialog, or with `edit` the dialog on that key of the table (PostgreSQL).
    pub(in crate::app) fn open_fk_form(&mut self, edit: Option<usize>) {
        let Some(own) = self.structure().cloned() else { return };
        let from = edit.and_then(|i| own.foreign_keys.get(i).cloned());
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
        self.modal = Some(Modal::FkEdit(Box::new(FkForm {
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
        })));
    }

    pub(in crate::app) fn stage_drop_fk(&mut self, i: usize) {
        let Some(name) = self.structure().and_then(|s| s.foreign_keys.get(i)).map(|k| k.name.clone()) else { return };
        let mut staged = self.staged().to_vec();
        let pg = self.t().is_some_and(|t| self.is_pg(t.sel.conn));
        if staged.iter().any(|c| matches!(c, Change::DropFk(n) if ddl::fk_name_is(pg, n, &name))) {
            return;
        }
        staged.push(Change::DropFk(name));
        self.set_staged(staged);
    }

    /// The tables a key can point at in a namespace (a database, or a schema of `db` on PostgreSQL), views left out. None
    /// while the list is still loading.
    pub(in crate::app) fn fk_tables(&mut self, conn: u64, db: &str, schema: &str) -> Option<Vec<String>> {
        if self.is_pg(conn) {
            self.pg_load_tables(conn, db, schema);
            let key = Sel { conn, db: db.to_string(), schema: schema.to_string(), coll: String::new() }.nskey();
            return Some(self.conn(conn)?.tables_in(db, schema)?.iter().filter(|t| !self.views.has(conn, &key, t)).cloned().collect());
        }
        let list = self.conn(conn)?.dbs.iter().find(|d| d.0 == schema)?.1.clone();
        match list {
            Some(l) => Some(l.into_iter().filter(|t| !self.views.has(conn, schema, t)).collect()),
            None => {
                if self.sql_asked.insert((conn, schema.to_string())) {
                    if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
                        self.worker.sql_tables(conn, sql, schema.to_string());
                    }
                }
                None
            }
        }
    }

    pub(in crate::app) fn fk_dialog(&mut self, ui: &mut egui::Ui, f: &mut FkForm, keep: &mut bool) {
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let (conn, base_db, table) = (sel.conn, sel.db.clone(), sel.coll.clone());
        let pg = f.pg;
        // The session of the tab's database and the namespace its table is in.
        let has_sql = self.conn(conn).is_some_and(|c| c.sql.is_some());
        let schema = if pg { self.sel_schema(&sel) } else { base_db.clone() };
        ui.set_width(600.0);
        dialog_title(ui, if f.from.is_some() { "Edit relation" } else { "Add relation" });
        ui.label(RichText::new(&table).size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);

        let dbs: Vec<String> = if pg {
            // Every schema of this database that holds user objects, and the table's own.
            self.conn(conn).and_then(|c| c.pg.get(&base_db)).map(|p| p.schemas.iter().filter(|s| !s.system || s.name == schema).map(|s| s.name.clone()).collect()).unwrap_or_else(|| vec![schema.clone()])
        } else {
            self.conn(conn).map(|c| c.dbs.iter().map(|d| d.0.clone()).filter(|d| *d == schema || !db::sql::create::SYSTEM_SCHEMAS.contains(&d.as_str())).collect()).unwrap_or_default()
        };
        let tables = self.fk_tables(conn, &base_db, &f.ref_db);
        let parent_cols: Vec<String> = f.parent.as_ref().map(|p| p.columns.iter().map(|c| c.name.clone()).collect()).unwrap_or_default();
        let mut remove = None;
        let mut add_pair = false;

        egui::Grid::new("fkform").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 20.0).show(ui, |ui| {
            form_label(ui, "Name");
            let r = ui.add(egui::TextEdit::singleline(&mut f.name).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).hint_text("fk_orders_customer"));
            if r.changed() {
                f.named = true;
            }
            ui.end_row();

            form_label(ui, "References");
            ui.horizontal(|ui| {
                let before = (f.ref_db.clone(), f.ref_table.clone());
                let combo = egui::ComboBox::from_id_salt("fkdb").width(150.0).selected_text(f.ref_db.as_str()).show_ui(ui, |ui| {
                    menu_look(ui);
                    for d in &dbs {
                        ui.selectable_value(&mut f.ref_db, d.clone(), d.as_str());
                    }
                });
                combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, if pg { "Referenced schema" } else { "Referenced database" }));
                if f.ref_db != before.0 {
                    f.ref_table.clear();
                    for p in &mut f.pairs {
                        p.1.clear();
                    }
                }
                let shown = if f.ref_table.is_empty() { if tables.is_none() { "Loading tables..." } else { "Pick a table" } } else { f.ref_table.as_str() };
                let combo = egui::ComboBox::from_id_salt("fktable").width(ui.available_width()).selected_text(shown).show_ui(ui, |ui| {
                    menu_look(ui);
                    for t in tables.iter().flatten() {
                        ui.selectable_value(&mut f.ref_table, t.clone(), t.as_str());
                    }
                });
                combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Referenced table"));
                if f.ref_table != before.1 {
                    for p in &mut f.pairs {
                        p.1.clear();
                    }
                }
            });
            ui.end_row();

            form_label(ui, "Columns");
            ui.vertical(|ui| {
                let many = f.pairs.len() > 1;
                for (i, p) in f.pairs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let combo = egui::ComboBox::from_id_salt(("fkown", i)).width(170.0).selected_text(if p.0.is_empty() { "Column" } else { p.0.as_str() }).show_ui(ui, |ui| {
                            menu_look(ui);
                            for c in &f.own_cols {
                                ui.selectable_value(&mut p.0, c.clone(), c.as_str());
                            }
                        });
                        combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Column {}", i + 1)));
                        ui.label(RichText::new(type_text(&f.own, &p.0)).monospace().size(TXT_SMALL).color(FAINT()));
                        ui.label(RichText::new("to").color(MUTED()));
                        let combo = egui::ComboBox::from_id_salt(("fkref", i)).width(170.0).selected_text(if p.1.is_empty() { "Column" } else { p.1.as_str() }).show_ui(ui, |ui| {
                            menu_look(ui);
                            for c in &parent_cols {
                                ui.selectable_value(&mut p.1, c.clone(), c.as_str());
                            }
                        });
                        combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Referenced column {}", i + 1)));
                        if let Some(par) = &f.parent {
                            ui.label(RichText::new(type_text(par, &p.1)).monospace().size(TXT_SMALL).color(FAINT()));
                        }
                        if many && quiet(ui, "Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if quiet_enabled(ui, f.pairs.len() < f.own_cols.len().min(16), "+ Column").on_hover_text("Add another column to a composite key").clicked() {
                    add_pair = true;
                }
            });
            ui.end_row();

            for (label, value) in [("On update", &mut f.on_update), ("On delete", &mut f.on_delete)] {
                form_label(ui, label);
                let mut i = action_index(value, pg);
                let items: Vec<(usize, &str)> = (0..actions(pg).len()).map(|k| (k, action_label(k))).collect();
                segmented(ui, &mut i, &items);
                *value = actions(pg)[i].to_string();
                ui.end_row();
            }
            if pg {
                form_label(ui, "Options");
                ui.vertical(|ui| {
                    ui.checkbox(&mut f.match_full, "Match full").on_hover_text("A key with several columns must be all NULL or all set. The default lets any NULL column skip the check.");
                    ui.checkbox(&mut f.deferrable, "Deferrable").on_hover_text("Lets a transaction postpone the check to its end.");
                    ui.add_enabled(f.deferrable, egui::Checkbox::new(&mut f.deferred, "Initially deferred"));
                    ui.checkbox(&mut f.not_valid, "Do not check the rows already there (NOT VALID)").on_hover_text("The key holds for new and changed rows only, and the existing ones are left as they are.");
                });
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            f.pairs.remove(i);
        }
        if add_pair {
            f.pairs.push((String::new(), String::new()));
        }
        // The default name follows the picks until the person types one.
        if !f.named {
            f.name = match f.pairs.first() {
                Some((c, _)) if !c.is_empty() => format!("fk_{table}_{}", f.pairs.iter().map(|p| p.0.as_str()).collect::<Vec<_>>().join("_")),
                _ => String::new(),
            };
        }

        // The referenced table's structure: its own when the key points back at this table.
        if !f.ref_table.is_empty() && (f.ref_db.clone(), f.ref_table.clone()) != f.asked {
            f.asked = (f.ref_db.clone(), f.ref_table.clone());
            f.parent = None;
            f.parent_err = None;
            f.orphans = None;
            f.orphan_for = None;
            f.sample.clear();
            if f.ref_db == schema && f.ref_table == table {
                f.parent = Some(f.own.clone());
                f.parent_req = 0;
            } else if let Some(sql) = self.sql_target(&sel).map(|t| t.0) {
                self.req += 1;
                f.parent_req = self.req;
                self.worker.sql_structure(self.req, sql, f.ref_db.clone(), f.ref_table.clone());
            }
        }

        let key = f.key(&schema);
        let loading = !f.ref_table.is_empty() && f.parent.is_none() && f.parent_err.is_none();
        let mut problem = None;
        let mut hard = false;
        if let Some(e) = &f.parent_err {
            problem = Some(format!("Could not read {}: {e}", f.ref_table));
            hard = true;
        } else if let (Some(k), Some(par)) = (&key, &f.parent) {
            problem = ddl::fk_problem(&f.own, &table, par, k, f.strict);
            hard = problem.is_some();
            if problem.is_none() {
                let taken = |n: &str| f.own.foreign_keys.iter().any(|x| ddl::fk_name_is(pg, &x.name, n) && f.from.as_ref().is_none_or(|o| o.name != x.name)) || f.rest.iter().any(|c| matches!(c, Change::AddFk(x) if ddl::fk_name_is(pg, &x.name, n))) || f.own.constraints.iter().any(|x| x.name == n);
                if taken(&k.name) {
                    problem = Some(format!("The table already has a foreign key called {}.", k.name));
                    hard = true;
                }
            }
        }
        // The orphan check runs once per set of columns, on its own request, and a big table waits for a click. The
        // name and the actions do not change its answer, so typing them asks nothing.
        let okey = key.as_ref().map(OrphanKey::of);
        if let (Some(k), None, Some(ok)) = (&key, &problem, &okey) {
            if f.orphan_for.as_ref() != Some(ok) {
                let big = f.own.status.rows.is_some_and(|n| n > CHECK_ROWS);
                if f.parent.is_some() && (!big || f.check) {
                    if let Some(sql) = self.sql_target(&sel).map(|t| t.0) {
                        self.req += 1;
                        (f.orphan_req, f.orphan_for, f.orphans) = (self.req, okey.clone(), None);
                        f.sample.clear();
                        self.worker.sql_orphans(self.req, sql, schema.clone(), table.clone(), k.clone());
                    }
                }
            }
        }

        let mut text: Result<String, String> = Err(String::new());
        let mut all = f.rest.clone();
        if let Some(k) = &key {
            match &f.from {
                Some(old) => all.extend(edit_changes(old, k)),
                None => all.push(Change::AddFk(k.clone())),
            }
            text = if let Some(p) = &problem {
                Err(p.clone())
            } else if loading {
                Err("Reading the referenced table...".into())
            } else if k.name.is_empty() {
                Err("Name the relation.".into())
            } else if all.len() == f.rest.len() {
                Err("Nothing is changed.".into())
            } else {
                PlanMemo::of(&mut f.memo, self, &f.own, &all).plan.as_ref().map(|p| p.sql()).map_err(Clone::clone)
            };
        } else if let Some(p) = &problem {
            text = Err(p.clone());
        }

        ui.add_space(2.0);
        ui.scope(|ui| {
            ui.set_min_height(ORPHAN_BOX_H);
            ui.set_width(ui.available_width());
            if let (Some(k), None) = (&key, &problem) {
                let (col, rc) = (k.cols.join(", "), format!("{}.{}", k.ref_table, k.ref_cols.join(", ")));
                match &f.orphans {
                    Some(Ok(n)) if *n > 0 && f.orphan_for == okey => {
                        egui::Frame::new().fill(tint(WARNING(), 34)).corner_radius(R_MD).inner_margin(10).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let them = if *n == 1 { "it" } else { "them" };
                            let fix = if k.not_valid { "They stay as they are.".to_string() } else if pg { format!("Fix {them} first, or add the key as NOT VALID.") } else { format!("Fix {them} first or the ALTER fails.") };
                            let mut msg = format!("{n} row{} {} {col} missing from {rc}. {fix}", if *n == 1 { "" } else { "s" }, if *n == 1 { "has a value in" } else { "have values in" });
                            if !f.sample.is_empty() {
                                msg.push_str(&format!(" For example {}.", f.sample.join(", ")));
                            }
                            ui.label(RichText::new(msg).size(TXT_SMALL).color(WARNING()));
                        });
                    }
                    Some(Ok(_)) => {
                        ui.label(RichText::new("Every existing row has a match.").size(TXT_SMALL).color(FAINT()));
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(format!("Could not check the existing rows: {e}")).size(TXT_SMALL).color(FAINT()));
                    }
                    None if f.orphan_for.is_some() => {
                        ui.label(RichText::new("Checking the existing rows...").size(TXT_SMALL).color(FAINT()));
                    }
                    None if has_sql && f.own.status.rows.is_some_and(|n| n > CHECK_ROWS) => {
                        if quiet(ui, "Check existing rows").on_hover_text("Joins the whole table, which can take a while on a big one").clicked() {
                            f.check = true;
                        }
                    }
                    None => {}
                }
            }
        });
        ui.add_space(6.0);
        caption(ui, "SQL TO RUN");
        ui.add_space(4.0);
        match &text {
            Ok(t) => sql_block(ui, "fk-sql", t, 150.0),
            Err(e) => {
                ui.label(RichText::new(if e.is_empty() { "Pick the table and the columns." } else { e.as_str() }).size(TXT_SMALL).color(if hard { ALERT() } else { FAINT() }));
            }
        }
        if let Some(st) = self.structure() {
            for w in ddl::warnings(st, &all) {
                ui.label(RichText::new(w).size(TXT_SMALL).color(WARNING()));
            }
        }
        let (mut stage, mut now) = (false, false);
        dialog_actions(ui, |ui| {
            stage = ui.add_enabled(text.is_ok(), primary_button(ui, if f.from.is_some() { "Change relation" } else { "Add relation" }, PRIMARY())).clicked();
            let busy = self.applying();
            // The dialog's own count of the rows with no match holds a read lock on the table until it answers, which a change that
            // locks the table harder would meet as another session.
            let counting = f.orphan_for.is_some() && f.orphans.is_none();
            now = ui.add_enabled(text.is_ok() && !self.sel_is_view() && !busy && !counting, egui::Button::new("Apply now")).on_hover_text("Stage this change and run everything staged. A locked connection asks to unlock first.").on_disabled_hover_text(if busy { BUSY_NOTE } else if counting { "Checking the existing rows first" } else { "A view cannot be changed here" }).clicked();
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

    /// Opens the row a key cell points to: the referenced table, filtered to that row. A composite key uses every
    /// column. `row` is the document of the clicked row. A tab of that table is reused only when it holds no filter,
    /// sort or clause of the person's, so none is lost; otherwise the row opens in a tab of its own. The tab loads once.
    pub(in crate::app) fn open_fk_row(&mut self, fk: &ForeignKey, row: &Document) {
        let Some(conn) = self.t().map(|t| t.sel.conn) else { return };
        // The editor belongs to this tab's rows; it would stage into the wrong table once the tab changes.
        if self.sql_cell.is_some() {
            return self.warn("Finish or cancel the cell edit first.");
        }
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        let cols = self.t().and_then(|t| t.sql.as_ref()).map(|s| s.cols.clone()).unwrap_or_default();
        let text = match fk_row_filter(d, bs, fk, &cols, row) {
            Ok(t) => t,
            Err(why) => return self.warn(why),
        };
        let base = self.t().map(|t| t.sel.clone()).unwrap_or_default();
        let sel = sql::ns::with_ns(self.dialect_of(conn), &base, &fk.ref_db, fk.ref_table.clone());
        self.reset_doc_state();
        match self.tabs.iter().position(|t| t.sel == sel && !has_clauses(t)) {
            Some(i) => self.set_cur(i),
            None => {
                let engine = self.conn(conn).map(|c| c.meta.engine).unwrap_or_default();
                self.tabs.push(Tab::new(sel).for_engine(engine));
                self.set_cur(self.tabs.len() - 1);
            }
        }
        let Some(t) = self.tm() else { return };
        t.view = View::Documents;
        t.filter_txt = text;
        t.page = 0;
        self.reload();
    }

    /// QUARRY_TEST_SQL=addrel: the dialog filled as in the design, the currency column of orders pointing at
    /// currencies.code. Offline, so the referenced table's structure and the orphan count are set here.
    pub(in crate::app) fn sql_rel_demo(&mut self, which: &str) {
        if which != "addrel" {
            return;
        }
        self.open_fk_form(None);
        let Some(Modal::FkEdit(f)) = self.modal.as_mut() else { return };
        use db::sql::structure::{Column, Index, IndexCol};
        let code = Column { name: "code".into(), full_type: "char(3)".into(), collation: Some("utf8mb4_0900_ai_ci".into()), key: "PRI".into(), ..Default::default() };
        let label = Column { name: "label".into(), full_type: "varchar(60)".into(), nullable: true, collation: Some("utf8mb4_0900_ai_ci".into()), ..Default::default() };
        let primary = Index { name: "PRIMARY".into(), unique: true, cols: vec![IndexCol { name: "code".into(), ..Default::default() }], kind: "BTREE".into(), ..Default::default() };
        f.parent = Some(Structure { columns: vec![code, label], indexes: vec![primary], ..Default::default() });
        f.ref_table = "currencies".into();
        f.asked = (f.ref_db.clone(), f.ref_table.clone());
        f.pairs = vec![("currency".into(), "code".into())];
        f.name = "fk_orders_currency".into();
        f.named = true;
        f.on_update = "CASCADE".into();
        f.on_delete = "SET NULL".into();
        // The demo's currency column is NOT NULL, which SET NULL cannot serve, so the design's example needs it nullable.
        if let Some(c) = f.own.columns.iter_mut().find(|c| c.name == "currency") {
            c.nullable = true;
        }
        // The design adds the key that the relations demo already lists, so the form starts without it.
        f.own.foreign_keys.retain(|k| k.name != "fk_orders_currency");
        let k = f.key("shop");
        (f.orphan_for, f.orphans) = (k.as_ref().map(OrphanKey::of), Some(Ok(3)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::sql::value::Kind;
    use db::sql::{Col, Dialect};

    fn key(cols: &[&str]) -> ForeignKey {
        ForeignKey { name: "fk".into(), cols: cols.iter().map(|c| (*c).into()).collect(), ref_table: "parent".into(), ref_cols: cols.iter().map(|c| format!("p_{c}")).collect(), ..Default::default() }
    }

    /// Review finding 13: a FLOAT key cannot be matched by the rounded value the grid holds.
    #[test]
    fn a_float_key_gets_no_filter() {
        let row = mongodb::bson::doc! { "a": 1, "f": 0.1 };
        let cols = [Col::plain("a", "", Kind::Int, true), Col::plain("f", "", Kind::Float, true)];
        assert_eq!(fk_row_filter(Dialect::MySql, true, &key(&["a"]), &cols, &row).unwrap(), "`p_a` = 1");
        assert!(fk_row_filter(Dialect::MySql, true, &key(&["a", "f"]), &cols, &row).unwrap_err().contains("floating point"));
        let null = mongodb::bson::doc! { "a": Bson::Null };
        assert!(fk_row_filter(Dialect::MySql, true, &key(&["a"]), &cols, &null).unwrap_err().contains("NULL"));
    }

    /// Review finding 13: a tab with the person's own clauses is not replaced by the row's filter.
    #[test]
    fn a_tab_with_clauses_is_not_reused() {
        let sel = Sel { conn: 1, db: "shop".into(), coll: "orders".into(), ..Default::default() };
        let mut t = Tab::new(sel);
        assert!(!has_clauses(&t));
        t.sort_txt = "id DESC".into();
        assert!(has_clauses(&t));
        t.sort_txt.clear();
        t.filter_txt = "status = 'paid'".into();
        assert!(has_clauses(&t));
    }

    /// Review finding 9: the orphan count is asked again only when the columns or the table change.
    #[test]
    fn typing_a_name_does_not_count_orphans_again() {
        let a = key(&["a"]);
        let b = ForeignKey { name: "fk_typed".into(), on_delete: "CASCADE".into(), ..a.clone() };
        assert_eq!(ddl::orphan_key(&a), ddl::orphan_key(&b));
        assert_ne!(ddl::orphan_key(&a), ddl::orphan_key(&key(&["b"])));
    }
}
