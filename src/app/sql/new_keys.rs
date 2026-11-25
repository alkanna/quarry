//! The UNIQUE keys, CHECKs, foreign keys and indexes of a table being made, which the PostgreSQL and the MySQL New table dialogs
//! collect the same way. The statements are not written here: each dialect turns the lists into its own spec.

use crate::app::*;
use db::sql::ddl;
use db::sql::Dialect;
pub(crate) use quarry_core::state::sql::new_keys::*;

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
    /// The schemas (databases on MySQL) a foreign key of the new table can point into: the system ones are left out, except the table's own.
    fn key_schemas(&self, env: &KeyEnv) -> Vec<String> {
        if env.d == Dialect::Postgres {
            self.conn(env.conn).and_then(|c| c.pg.get(env.database)).map(|p| p.schemas.iter().filter(|s| !s.system || s.name == env.schema).map(|s| s.name.clone()).collect()).unwrap_or_else(|| vec![env.schema.to_string()])
        } else {
            self.conn(env.conn).map(|c| c.dbs.iter().map(|x| x.0.clone()).filter(|x| *x == env.schema || !db::sql::create::SYSTEM_SCHEMAS.contains(&x.as_str())).collect()).unwrap_or_default()
        }
    }

    /// The UNIQUE constraints over several columns, the CHECKs, the foreign keys and the indexes of the new table, one row each with its
    /// name (empty lets the server name it), what it holds, and a remove button. A foreign key's parent is read when it is picked.
    /// `cols` are the table's column names as typed.
    pub(in crate::app) fn constraint_list(&mut self, ui: &mut egui::Ui, keys: &mut NewKeys, env: &KeyEnv, cols: &[String]) {
        let pg = env.d == Dialect::Postgres;
        if keys.is_empty() {
            let hint = if pg { "None yet. A column's Unique box is a single-column UNIQUE; add one over several columns, a CHECK, a foreign key or an index here." } else { "None yet. Add a UNIQUE key over one or more columns, a CHECK, a foreign key or an index here." };
            ui.label(RichText::new(hint).size(TXT_SMALL).color(FAINT()));
            return;
        }
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
                                    // Listed only while the menu is open.
                                    for s in &self.key_schemas(env) {
                                        ui.selectable_value(&mut k.key.ref_db, s.clone(), s.as_str());
                                    }
                                });
                                r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Referenced {} {n}", if pg { "schema" } else { "database" })));
                                let list = tables.get(&k.key.ref_db).and_then(|t| t.as_deref());
                                let shown = if k.own { this.clone() } else if !k.key.ref_table.is_empty() { k.key.ref_table.clone() } else if list.is_none() { "Loading tables...".into() } else { "Pick a table".into() };
                                let r = egui::ComboBox::from_id_salt((salt, "fk-table", i)).width(160.0).selected_text(shown).show_ui(ui, |ui| {
                                    menu_look(ui);
                                    if k.key.ref_db == env.schema && ui.selectable_label(k.own, this.as_str()).clicked() {
                                        (k.own, k.key.ref_table) = (true, String::new());
                                    }
                                    for t in list.unwrap_or_default() {
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
                            let lines = k.key.cols.len();
                            let mut cut = None;
                            for j in 0..lines {
                                ui.horizontal(|ui| {
                                    for (side, label) in [(0, "Column"), (1, "Referenced column")] {
                                        let v = if side == 0 { &mut k.key.cols[j] } else { &mut k.key.ref_cols[j] };
                                        let r = egui::ComboBox::from_id_salt((salt, "fk-col", i, j, side)).width(150.0).selected_text(if v.is_empty() { "Column".to_string() } else { v.clone() }).show_ui(ui, |ui| {
                                            menu_look(ui);
                                            // The table's own columns, or the parent's as the server gave them.
                                            if side == 0 || k.own {
                                                for c in cols {
                                                    ui.selectable_value(v, c.clone(), c.as_str());
                                                }
                                            } else if let Some(p) = &k.parent {
                                                for c in &p.columns {
                                                    ui.selectable_value(v, c.name.clone(), c.name.as_str());
                                                }
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
                                        // The dialect's default first.
                                        let first = if pg { "NO ACTION" } else { "RESTRICT" };
                                        for a in std::iter::once(&first).chain(super::rel::actions(pg).iter().filter(|a| **a != first)) {
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
                                let mut unique = x.kind == ddl::IndexKind::Unique;
                                ui.checkbox(&mut unique, "Unique").widget_info(|| egui::WidgetInfo::labeled(egui::Role::CheckBox, true, format!("Unique index {n}")));
                                x.kind = if unique { ddl::IndexKind::Unique } else { ddl::IndexKind::Index };
                            } else {
                                // FULLTEXT and SPATIAL are MySQL's own kinds; UNIQUE shares the box PostgreSQL has.
                                let mut now = x.kind;
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
                                x.kind = now;
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
    use db::sql::structure::{ForeignKey, Structure};
    use db::sql::structure::{Column, Index, IndexCol, Status};

    fn parent(engine: &str) -> Structure {
        let id = Column { name: "id".into(), full_type: "int unsigned".into(), nullable: false, ..Default::default() };
        let pk = Index { name: "PRIMARY".into(), unique: true, cols: vec![IndexCol { name: "id".into(), ..Default::default() }], kind: "BTREE".into(), ..Default::default() };
        Structure { columns: vec![id], indexes: vec![pk], status: Status { engine: engine.into(), ..Default::default() }, ..Default::default() }
    }

    #[test]
    fn a_key_needs_an_innodb_parent_and_waits_for_it() {
        let own = Structure { columns: vec![Column { name: "c".into(), full_type: "int unsigned".into(), nullable: false, ..Default::default() }], ..Default::default() };
        let mut keys = NewKeys::default();
        keys.fks.push(NewFk::new("shop", Dialect::MySql));
        let key = ForeignKey { name: "f".into(), cols: vec!["c".into()], ref_db: "shop".into(), ref_table: "p".into(), ref_cols: vec!["id".into()], on_update: "RESTRICT".into(), on_delete: "RESTRICT".into(), ..Default::default() };
        let problem = |keys: &NewKeys, engine: &str| keys.key_problem(std::slice::from_ref(&key), &own, "t", false, Some(engine));
        assert_eq!(problem(&keys, "InnoDB").map(|p| p.1), Some(true), "the parent is still being read");
        keys.fks[0].parent = Some(parent("MyISAM"));
        assert!(problem(&keys, "InnoDB").unwrap().0.contains("MyISAM table"));
        keys.fks[0].parent = Some(parent("InnoDB"));
        assert_eq!(problem(&keys, "InnoDB"), None);
        // PostgreSQL has no engine to ask about
        keys.fks[0].parent = Some(parent("MyISAM"));
        assert_eq!(keys.key_problem(std::slice::from_ref(&key), &own, "t", false, None), None);
    }
}
