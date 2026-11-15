//! The UNIQUE keys, CHECKs, foreign keys and indexes of a table being made, which the PostgreSQL and the MySQL New table dialogs
//! collect the same way. The statements are not written here: each dialect turns the lists into its own spec.

use crate::app::*;
use db::sql::ddl;
use db::sql::pg::create::{Check, NewIndex, Unique};
use db::sql::structure::{ForeignKey, Structure};
use db::sql::Dialect;

/// A foreign key of the builder, with the parent's structure as the server gave it, read on a request of its own like the Add
/// relation dialog's. `key.cols` and `key.ref_cols` are kept the same length, one pair per line, empty until picked.
#[derive(Clone)]
pub(in crate::app) struct PgFk {
    pub(in crate::app) key: ForeignKey,
    /// The key points at the table being made, which follows the name typed.
    pub(in crate::app) own: bool,
    pub(in crate::app) parent: Option<Structure>,
    /// The schema and table `parent` was asked for, and the request that carries the answer.
    asked: (String, String),
    req: u64,
    err: Option<String>,
}

impl PgFk {
    pub(in crate::app) fn new(schema: &str, d: Dialect) -> Self {
        let action = if d == Dialect::Postgres { "NO ACTION" } else { "RESTRICT" };
        let key = ForeignKey { ref_db: schema.to_string(), cols: vec![String::new()], ref_cols: vec![String::new()], on_update: action.into(), on_delete: action.into(), ..Default::default() };
        PgFk { key, own: false, parent: None, asked: Default::default(), req: 0, err: None }
    }
}

/// The actions of a key, the dialect's default first.
fn fk_actions(d: Dialect) -> &'static [&'static str] {
    const PG: [&str; 5] = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];
    const MY: [&str; 4] = ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"];
    if d == Dialect::Postgres { &PG } else { &MY }
}

/// What the builder collects besides the columns.
#[derive(Default)]
pub(in crate::app) struct NewKeys {
    /// The UNIQUE constraints over several columns; one over a single column is also its column's Unique box on PostgreSQL.
    pub(in crate::app) uniques: Vec<Unique>,
    pub(in crate::app) checks: Vec<Check>,
    pub(in crate::app) fks: Vec<PgFk>,
    pub(in crate::app) indexes: Vec<NewIndex>,
}

impl NewKeys {
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.uniques.is_empty() && self.checks.is_empty() && self.fks.is_empty() && self.indexes.is_empty()
    }

    /// Every pick of a column by name in the UNIQUEs, the indexes and the keys (the parent side only when the key points at this
    /// table), so a rename or a removal reaches all of them.
    fn picks_mut(&mut self) -> impl Iterator<Item = &mut String> {
        let lists = self.uniques.iter_mut().map(|u| &mut u.cols).chain(self.indexes.iter_mut().map(|i| &mut i.cols));
        let keys = self.fks.iter_mut().flat_map(|k| {
            let own = k.own;
            k.key.cols.iter_mut().chain(k.key.ref_cols.iter_mut().filter(move |_| own))
        });
        lists.flat_map(|v| v.iter_mut()).chain(keys)
    }

    /// A column was renamed from `from`; its picks follow. An emptied name is a removal, because an empty pick on a key line means
    /// nothing picked.
    pub(in crate::app) fn renamed(&mut self, from: &str, to: &str) {
        if to.is_empty() {
            return self.forget(from);
        }
        self.picks_mut().filter(|p| *p == from).for_each(|p| *p = to.to_string());
    }

    /// Takes a removed column out of every list. A UNIQUE or index pick list hides a name it does not offer, so a stale one could
    /// neither be seen nor removed and a new column of that name would silently take its place. A key line is kept and left unpicked.
    pub(in crate::app) fn forget(&mut self, name: &str) {
        for v in self.uniques.iter_mut().map(|u| &mut u.cols).chain(self.indexes.iter_mut().map(|i| &mut i.cols)) {
            v.retain(|c| c != name);
        }
        self.picks_mut().filter(|p| *p == name).for_each(String::clear);
    }

    /// The answer to a parent's structure read, when it is for one of these keys.
    pub(in crate::app) fn parent_loaded(&mut self, req: u64, res: &db::Res<Structure>) -> bool {
        let Some(k) = self.fks.iter_mut().find(|k| k.req == req && req != 0) else { return false };
        match res {
            Ok(s) => (k.parent, k.err) = (Some(s.clone()), None),
            Err(e) => (k.parent, k.err) = (None, Some(e.clone())),
        }
        true
    }

    /// The first thing stopping a key that the statements cannot see: a part not picked, a parent not read yet or one that does not
    /// fit. The second value is true while a parent is still loading, which is not a mistake to point at. `own` is the table being
    /// made as `fks` (the same keys, as the spec has them) would find it. With an `engine` (MySQL) the keys also need InnoDB on both
    /// sides, which the server only says as "cannot add a foreign key constraint", and a key at the table itself is checked too.
    pub(in crate::app) fn key_problem(&self, fks: &[ForeignKey], own: &Structure, table: &str, whole_key: bool, engine: Option<&str>) -> Option<(String, bool)> {
        if let Some(e) = engine.filter(|e| !fks.is_empty() && !e.eq_ignore_ascii_case("InnoDB")) {
            return Some((format!("Foreign keys need an InnoDB table, and {} is not one.", if e.is_empty() { "this engine" } else { e }), false));
        }
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

/// Picks columns in key order, one toggle per column of the table: a click adds a column at the end or takes it out, and a picked
/// one shows its place in the key. `label` names the row for the screen reader.
fn cols_pick(ui: &mut egui::Ui, label: &str, all: &[String], picked: &mut Vec<String>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for c in all {
            let at = picked.iter().position(|p| p == c);
            let text = match at {
                Some(i) => format!("{} {c}", i + 1),
                None => c.clone(),
            };
            let r = ui.selectable_label(at.is_some(), RichText::new(text).monospace().size(TXT_SMALL));
            // The row's own name in front, so a screen reader and a test tell its column from the same name elsewhere on the page.
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, at.is_some(), format!("{label} {c}")));
            if r.clicked() {
                match at {
                    Some(i) => {
                        picked.remove(i);
                    }
                    None => picked.push(c.clone()),
                }
            }
        }
        if all.is_empty() {
            ui.label(RichText::new("Name the columns first").size(TXT_SMALL).color(FAINT()));
        }
    });
}

fn name_field(ui: &mut egui::Ui, text: &mut String, label: String) {
    ui.add(egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).desired_width(150.0).hint_text("server names it")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, &label));
}

/// What the lists need to know about the table being made.
pub(in crate::app) struct KeyEnv<'a> {
    pub(in crate::app) conn: u64,
    /// The database the session is bound to for reading a parent: the database on PostgreSQL, the schema on MySQL.
    pub(in crate::app) database: &'a str,
    pub(in crate::app) schema: &'a str,
    /// The name typed for the table.
    pub(in crate::app) name: &'a str,
    pub(in crate::app) d: Dialect,
}

impl App {
    /// The UNIQUE constraints over several columns, the CHECKs, the foreign keys and the indexes of the new table, one row each with its
    /// name (empty lets the server name it), what it holds, and a remove button. A foreign key's parent is read when it is picked.
    /// `cols` are the table's column names as typed.
    pub(in crate::app) fn constraint_list(&mut self, ui: &mut egui::Ui, keys: &mut NewKeys, env: &KeyEnv, cols: &[String]) {
        let (pg, d) = (env.d == Dialect::Postgres, env.d);
        if keys.is_empty() {
            let hint = if pg { "None yet. A column's Unique box is a single-column UNIQUE; add one over several columns, a CHECK, a foreign key or an index here." } else { "None yet. Add a UNIQUE key over one or more columns, a CHECK, a foreign key or an index here." };
            ui.label(RichText::new(hint).size(TXT_SMALL).color(FAINT()));
            return;
        }
        let schemas: Vec<String> = if pg {
            self.conn(env.conn).and_then(|c| c.pg.get(env.database)).map(|p| p.schemas.iter().filter(|s| !s.system || s.name == env.schema).map(|s| s.name.clone()).collect()).unwrap_or_else(|| vec![env.schema.to_string()])
        } else {
            self.conn(env.conn).map(|c| c.dbs.iter().map(|x| x.0.clone()).filter(|x| *x == env.schema || !db::sql::create::SYSTEM_SCHEMAS.contains(&x.as_str())).collect()).unwrap_or_default()
        };
        // The tables of each schema a key points at, loading the list the first time it is asked.
        let mut tables: HashMap<String, Option<Vec<String>>> = HashMap::new();
        for k in &keys.fks {
            if !tables.contains_key(&k.key.ref_db) {
                let t = self.fk_tables(env.conn, env.database, &k.key.ref_db);
                tables.insert(k.key.ref_db.clone(), t);
            }
        }
        let (mut drop_u, mut drop_c, mut drop_f, mut drop_i) = (None, None, None, None);
        let this = format!("{} (this table)", if env.name.is_empty() { "new table" } else { env.name });
        let salt = if pg { "pg-newtable" } else { "my-newtable" };
        card_frame().inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().id_salt((salt, "keys")).max_height(230.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new((salt, "keys-grid")).num_columns(3).spacing([10.0, 8.0]).show(ui, |ui| {
                    let remove = |ui: &mut egui::Ui, what: &str| ui.add(quiet_button(RichText::new("remove").size(TXT_CAPTION).color(FAINT()))).on_hover_text(format!("Remove this {what}")).clicked();
                    for (i, u) in keys.uniques.iter_mut().enumerate() {
                        let n = i + 1;
                        caption(ui, "UNIQUE");
                        ui.horizontal(|ui| {
                            name_field(ui, &mut u.name, format!("Unique name {n}"));
                            cols_pick(ui, &format!("Unique columns {n}"), cols, &mut u.cols);
                        });
                        if remove(ui, "constraint") {
                            drop_u = Some(i);
                        }
                        ui.end_row();
                    }
                    for (i, c) in keys.checks.iter_mut().enumerate() {
                        let n = i + 1;
                        caption(ui, "CHECK");
                        ui.horizontal(|ui| {
                            name_field(ui, &mut c.name, format!("Check name {n}"));
                            ui.add(egui::TextEdit::singleline(&mut c.expr).font(egui::TextStyle::Monospace).desired_width(330.0).hint_text("price >= 0")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Check expression {n}")));
                        });
                        if remove(ui, "check") {
                            drop_c = Some(i);
                        }
                        ui.end_row();
                    }
                    for (i, k) in keys.fks.iter_mut().enumerate() {
                        let n = i + 1;
                        caption(ui, "FOREIGN KEY");
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                name_field(ui, &mut k.key.name, format!("Foreign key name {n}"));
                                let before = (k.key.ref_db.clone(), k.key.ref_table.clone(), k.own);
                                let r = egui::ComboBox::from_id_salt((salt, "fk-schema", i)).width(110.0).selected_text(k.key.ref_db.as_str()).show_ui(ui, |ui| {
                                    menu_look(ui);
                                    for s in &schemas {
                                        ui.selectable_value(&mut k.key.ref_db, s.clone(), s.as_str());
                                    }
                                });
                                r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Referenced {} {n}", if pg { "schema" } else { "database" })));
                                let list = tables.get(&k.key.ref_db).cloned().flatten();
                                let shown = if k.own { this.clone() } else if !k.key.ref_table.is_empty() { k.key.ref_table.clone() } else if list.is_none() { "Loading tables...".into() } else { "Pick a table".into() };
                                let r = egui::ComboBox::from_id_salt((salt, "fk-table", i)).width(160.0).selected_text(shown).show_ui(ui, |ui| {
                                    menu_look(ui);
                                    if k.key.ref_db == env.schema && ui.selectable_label(k.own, this.as_str()).clicked() {
                                        (k.own, k.key.ref_table) = (true, String::new());
                                    }
                                    for t in list.iter().flatten() {
                                        if ui.selectable_label(!k.own && k.key.ref_table == *t, t.as_str()).clicked() {
                                            (k.own, k.key.ref_table) = (false, t.clone());
                                        }
                                    }
                                });
                                r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Referenced table {n}")));
                                if k.key.ref_db != before.0 {
                                    (k.own, k.key.ref_table) = (false, String::new());
                                }
                                if (k.key.ref_db.clone(), k.key.ref_table.clone(), k.own) != before {
                                    k.key.ref_cols.iter_mut().for_each(String::clear);
                                }
                            });
                            let parent_cols: Vec<String> = if k.own { cols.to_vec() } else { k.parent.as_ref().map(|p| p.columns.iter().map(|c| c.name.clone()).collect()).unwrap_or_default() };
                            let lines = k.key.cols.len();
                            let mut cut = None;
                            for j in 0..lines {
                                ui.horizontal(|ui| {
                                    for (side, list, label) in [(0, cols, "Column"), (1, parent_cols.as_slice(), "Referenced column")] {
                                        let v = if side == 0 { &mut k.key.cols[j] } else { &mut k.key.ref_cols[j] };
                                        let r = egui::ComboBox::from_id_salt((salt, "fk-col", i, j, side)).width(150.0).selected_text(if v.is_empty() { "Column".to_string() } else { v.clone() }).show_ui(ui, |ui| {
                                            menu_look(ui);
                                            for c in list {
                                                ui.selectable_value(v, c.clone(), c.as_str());
                                            }
                                        });
                                        r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Foreign key {n} {label} {}", j + 1)));
                                        if side == 0 {
                                            ui.label(RichText::new("to").color(MUTED()));
                                        }
                                    }
                                    if lines > 1 && quiet(ui, "Remove").clicked() {
                                        cut = Some(j);
                                    }
                                });
                            }
                            if let Some(j) = cut {
                                k.key.cols.remove(j);
                                k.key.ref_cols.remove(j);
                            }
                            ui.horizontal(|ui| {
                                if quiet_enabled(ui, k.key.cols.len() < cols.len().min(16), "+ Column").on_hover_text("Add another column to a composite key").clicked() {
                                    k.key.cols.push(String::new());
                                    k.key.ref_cols.push(String::new());
                                }
                                for (label, v) in [("On delete", &mut k.key.on_delete), ("On update", &mut k.key.on_update)] {
                                    ui.label(RichText::new(label).size(TXT_SMALL).color(MUTED()));
                                    let r = egui::ComboBox::from_id_salt((salt, "fk-act", i, label)).width(100.0).selected_text(v.as_str()).show_ui(ui, |ui| {
                                        menu_look(ui);
                                        for a in fk_actions(d) {
                                            ui.selectable_value(v, a.to_string(), *a);
                                        }
                                    });
                                    r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Foreign key {n} {label}")));
                                }
                            });
                            if pg {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut k.key.match_full, "Match full").on_hover_text("A key with several columns must be all NULL or all set. The default lets any NULL column skip the check.");
                                    ui.checkbox(&mut k.key.deferrable, "Deferrable").on_hover_text("Lets a transaction postpone the check to its end.");
                                    ui.add_enabled(k.key.deferrable, egui::Checkbox::new(&mut k.key.deferred, "Initially deferred"));
                                    if !k.key.deferrable {
                                        k.key.deferred = false;
                                    }
                                });
                            }
                        });
                        if remove(ui, "foreign key") {
                            drop_f = Some(i);
                        }
                        ui.end_row();
                    }
                    for (i, x) in keys.indexes.iter_mut().enumerate() {
                        let n = i + 1;
                        caption(ui, "INDEX");
                        ui.horizontal(|ui| {
                            name_field(ui, &mut x.name, format!("Index name {n}"));
                            cols_pick(ui, &format!("Index columns {n}"), cols, &mut x.cols);
                            if pg {
                                ui.checkbox(&mut x.unique, "Unique").widget_info(|| egui::WidgetInfo::labeled(egui::Role::CheckBox, true, format!("Unique index {n}")));
                            } else {
                                // FULLTEXT and SPATIAL are MySQL's own kinds; UNIQUE shares the box PostgreSQL has.
                                let mut now = if x.unique { ddl::IndexKind::Unique } else { x.kind };
                                let label = |k: ddl::IndexKind| match k {
                                    ddl::IndexKind::Unique => "Unique",
                                    ddl::IndexKind::Fulltext => "Fulltext",
                                    ddl::IndexKind::Spatial => "Spatial",
                                    _ => "Index",
                                };
                                let r = egui::ComboBox::from_id_salt((salt, "ix-kind", i)).width(90.0).selected_text(label(now)).show_ui(ui, |ui| {
                                    menu_look(ui);
                                    for k in [ddl::IndexKind::Index, ddl::IndexKind::Unique, ddl::IndexKind::Fulltext, ddl::IndexKind::Spatial] {
                                        ui.selectable_value(&mut now, k, label(k));
                                    }
                                });
                                r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Index kind {n}")));
                                (x.unique, x.kind) = (now == ddl::IndexKind::Unique, if now == ddl::IndexKind::Unique { ddl::IndexKind::Index } else { now });
                            }
                        });
                        if remove(ui, "index") {
                            drop_i = Some(i);
                        }
                        ui.end_row();
                    }
                });
            });
        });
        if let Some(i) = drop_u {
            keys.uniques.remove(i);
        }
        if let Some(i) = drop_c {
            keys.checks.remove(i);
        }
        if let Some(i) = drop_f {
            keys.fks.remove(i);
        }
        if let Some(i) = drop_i {
            keys.indexes.remove(i);
        }
        // Each key's parent, read once per pick on a request of its own.
        for k in keys.fks.iter_mut() {
            let want = (k.key.ref_db.clone(), k.key.ref_table.clone());
            if k.own || want.1.is_empty() || want == k.asked {
                continue;
            }
            (k.asked, k.parent, k.err) = (want.clone(), None, None);
            if let Some(sql) = self.pg_session(env.conn) {
                self.req += 1;
                k.req = self.req;
                self.worker.sql_structure(self.req, sql.at(env.database), want.0, want.1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::sql::structure::{Column, Index, IndexCol, Status};

    fn parent(engine: &str) -> Structure {
        let id = Column { name: "id".into(), full_type: "int unsigned".into(), nullable: false, ..Default::default() };
        let pk = Index { name: "PRIMARY".into(), unique: true, cols: vec![IndexCol { name: "id".into(), ..Default::default() }], kind: "BTREE".into(), ..Default::default() };
        Structure { columns: vec![id], indexes: vec![pk], status: Status { engine: engine.into(), ..Default::default() }, ..Default::default() }
    }

    #[test]
    fn a_key_needs_an_innodb_table_on_both_sides_and_waits_for_its_parent() {
        let own = Structure { columns: vec![Column { name: "c".into(), full_type: "int unsigned".into(), nullable: false, ..Default::default() }], ..Default::default() };
        let mut keys = NewKeys::default();
        keys.fks.push(PgFk::new("shop", Dialect::MySql));
        let key = ForeignKey { name: "f".into(), cols: vec!["c".into()], ref_db: "shop".into(), ref_table: "p".into(), ref_cols: vec!["id".into()], on_update: "RESTRICT".into(), on_delete: "RESTRICT".into(), ..Default::default() };
        let problem = |keys: &NewKeys, engine: &str| keys.key_problem(std::slice::from_ref(&key), &own, "t", false, Some(engine));
        assert_eq!(problem(&keys, "InnoDB").map(|p| p.1), Some(true), "the parent is still being read");
        assert!(problem(&keys, "MyISAM").unwrap().0.contains("InnoDB table"));
        keys.fks[0].parent = Some(parent("MyISAM"));
        assert!(problem(&keys, "InnoDB").unwrap().0.contains("MyISAM table"));
        keys.fks[0].parent = Some(parent("InnoDB"));
        assert_eq!(problem(&keys, "InnoDB"), None);
        // PostgreSQL has no engine to ask about
        keys.fks[0].parent = Some(parent("MyISAM"));
        assert_eq!(keys.key_problem(std::slice::from_ref(&key), &own, "t", false, None), None);
    }
}
