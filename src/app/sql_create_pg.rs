//! Creating and dropping on PostgreSQL: the New database, New schema and New table dialogs, the extra options of the drop
//! confirmation (CASCADE, WITH (FORCE)) and what the app does when an answer arrives. The statements come from
//! `db::sql::create_pg`. MySQL's dialogs are in `sql_create.rs`; this server has no engines, charsets or collations to pick
//! and a table is built from the structure editor's column model.

use super::sql_create::{list_and, pick, plural, right_actions, sql_block, DropForm, DropKind, NOTE_DB, NOTE_TABLE};
use super::*;
use db::sql::create::{Ddl, DropInfo};
use db::sql::create_pg::{self, TableSpec};
use db::sql::ddl::{ColDef, Dv};

pub(super) struct PgNewDb {
    conn: u64,
    name: String,
    /// None is the server's own default, template1.
    template: Option<String>,
    /// None while the server's list loads.
    pub(super) templates: Option<Vec<String>>,
    load_err: Option<String>,
    focused: bool,
}

pub(super) struct PgNewSchema {
    conn: u64,
    database: String,
    name: String,
    focused: bool,
}

/// One column of the builder: the structure editor's definition and the two marks the table level needs.
pub(super) struct PgCol {
    def: ColDef,
    pk: bool,
    unique: bool,
}

pub(super) struct PgNewTable {
    conn: u64,
    database: String,
    schema: String,
    name: String,
    cols: Vec<PgCol>,
    comment: String,
    show_sql: bool,
    focused: bool,
}

/// The types the picker offers. The type box takes any other name, and the server checks it before anything is made.
const TYPES: [&str; 17] = ["integer", "bigint", "smallint", "text", "varchar(255)", "numeric(10,2)", "boolean", "date", "timestamp", "timestamptz", "time", "uuid", "jsonb", "json", "real", "double precision", "bytea"];

impl PgNewTable {
    fn spec(&self) -> TableSpec {
        let cols: Vec<ColDef> = self.cols.iter().map(|c| ColDef { nullable: c.def.nullable && !c.pk && !c.def.auto_increment, ..c.def.clone() }).collect();
        TableSpec { schema: self.schema.clone(), name: self.name.clone(), cols, pk: self.cols.iter().filter(|c| c.pk).map(|c| c.def.name.clone()).collect(), uniques: self.cols.iter().filter(|c| c.unique).map(|c| vec![c.def.name.clone()]).collect(), comment: self.comment.clone() }
    }
}

/// The first column of a new table: an integer identity primary key, which is what most tables want.
fn id_col() -> PgCol {
    PgCol { def: ColDef { auto_increment: true, nullable: false, ..ColDef::new("id", "integer") }, pk: true, unique: false }
}

fn editor_label(ui: &mut egui::Ui, text: &mut String, width: f32, label: &str) -> egui::Response {
    let r = ui.add(egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).desired_width(width));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
    r
}

impl App {
    fn pg_session(&self, conn: u64) -> Option<db::sql::Db> {
        if self.demo {
            return None;
        }
        self.conn(conn)?.sql.clone()
    }

    /// The schema the sidebar lists for `db`, which is where a new table or a drop applies.
    fn pg_schema_of(&self, conn: u64, db: &str) -> String {
        Some(self.ns_of(conn, db)).filter(|s| !s.is_empty()).unwrap_or(PG_PUBLIC).to_string()
    }

    pub(super) fn pg_new_db(&mut self, conn: u64) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_new_db(conn));
        }
        if let Some(sql) = self.pg_session(conn) {
            self.worker.pg_templates(conn, sql);
        }
        self.modal = Some(Modal::PgNewDb(Box::new(PgNewDb { conn, name: String::new(), template: None, templates: None, load_err: None, focused: false })));
    }

    pub(super) fn pg_templates_done(&mut self, conn: u64, res: db::Res<Vec<String>>) {
        if let Some(Modal::PgNewDb(f)) = &mut self.modal {
            if f.conn == conn {
                match res {
                    Ok(t) => f.templates = Some(t),
                    Err(e) => f.load_err = Some(e),
                }
            }
        }
    }

    pub(super) fn pg_new_schema(&mut self, conn: u64, database: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_new_schema(conn, database));
        }
        self.modal = Some(Modal::PgNewSchema(Box::new(PgNewSchema { conn, database, name: String::new(), focused: false })));
    }

    pub(super) fn pg_new_table(&mut self, conn: u64, database: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_new_table(conn, database));
        }
        let schema = self.pg_schema_of(conn, &database);
        self.modal = Some(Modal::PgNewTable(Box::new(PgNewTable { conn, database, schema, name: String::new(), cols: vec![id_col()], comment: String::new(), show_sql: false, focused: false })));
    }

    /// Asks what a drop takes. `name` is the schema for a schema, the table or view for those, and empty for a database.
    pub(super) fn pg_drop_ask(&mut self, conn: u64, db: String, name: String, kind: DropKind) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_drop_ask(conn, db, name, kind));
        }
        let (database, schema, table) = match kind {
            DropKind::Database => (db, String::new(), String::new()),
            DropKind::Schema => (db, name, String::new()),
            _ => {
                let s = self.pg_schema_of(conn, &db);
                (db, s, name)
            }
        };
        if let Some(sql) = self.pg_session(conn) {
            self.worker.pg_drop_info(conn, sql, database.clone(), schema.clone(), table.clone());
        }
        self.modal = Some(Modal::SqlDrop(Box::new(DropForm { conn, database, schema, table, kind, typed: String::new(), opt: false, info: None })));
    }

    /// Runs a confirmed drop. A database first loses its tabs and its sidebar state, so nothing of this app asks the server for it
    /// while the drop runs, and the worker closes the pools; the other drops close their tabs when the server says it worked.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pg_drop_run(&mut self, conn: u64, sql: db::sql::Db, database: String, schema: String, table: String, kind: DropKind, opt: bool) {
        match kind {
            DropKind::Database => {
                self.remove_tabs(|t| t.sel.conn == conn && t.sel.db == database);
                self.open_dbs.remove(&(conn, database.clone()));
                self.sql_asked.remove(&(conn, database.clone()));
                self.forget_schema(conn, &database);
                self.pg_tables_asked.forget(conn, Some(&database), None);
                self.worker.pg_drop_db(conn, sql, database, opt, false);
            }
            DropKind::Schema => self.worker.pg_drop_schema(conn, sql.at(&database), schema, opt, false),
            DropKind::Table | DropKind::View => self.worker.pg_drop_table(conn, sql.at(&database), schema, table, kind == DropKind::View, opt, false),
            DropKind::Truncate => {}
        }
    }

    /// A create or drop finished: the sidebar lists and the tabs follow what the server has now.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pg_ddl_done(&mut self, conn: u64, database: String, schema: String, table: String, what: Ddl, res: db::Res<String>, dbs: Option<Vec<String>>) {
        match res {
            Ok(s) => self.ok(s),
            Err(e) => return self.err(e),
        }
        if let (Some(dbs), Some(c)) = (dbs, self.conns.iter_mut().find(|c| c.meta.id == conn)) {
            let old = std::mem::take(&mut c.dbs);
            c.dbs = dbs.into_iter().map(|d| (d.clone(), old.iter().find(|o| o.0 == d).and_then(|o| o.1.clone()))).collect();
        }
        // The tabs of a selection without a schema belong to the one the sidebar shows.
        let picked = self.pg_schema_of(conn, &database);
        let in_schema = |t: &Tab, s: &str| t.sel.conn == conn && t.sel.db == database && (if t.sel.schema.is_empty() { picked.as_str() } else { t.sel.schema.as_str() }) == s;
        match what {
            Ddl::CreatedDb => {
                // Its schemas load when the row opens, so nothing is marked asked.
                self.open_dbs.insert((conn, schema.clone()));
                self.reveal = Some((conn, schema, None));
            }
            Ddl::DroppedDb => {
                self.remove_tabs(|t| t.sel.conn == conn && t.sel.db == schema);
                self.open_dbs.remove(&(conn, schema.clone()));
                self.sql_asked.remove(&(conn, schema.clone()));
                self.forget_schema(conn, &schema);
                self.pg_tables_asked.forget(conn, Some(&schema), None);
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.pg.remove(&schema);
                    c.tables.remove(&schema);
                }
            }
            Ddl::CreatedSchema => {
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.pg.entry(database.clone()).or_default().picked = schema.clone();
                }
                self.pg_refresh(conn, &database);
            }
            Ddl::DroppedSchema => {
                self.remove_tabs(|t| in_schema(t, &schema));
                self.forget_schema(conn, &ns_key(&database, &schema));
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    if let Some(p) = c.pg.get_mut(&database).filter(|p| p.picked == schema) {
                        p.picked.clear();
                    }
                    if let Some(t) = c.tables.get_mut(&database) {
                        t.remove(&schema);
                    }
                }
                self.pg_refresh(conn, &database);
            }
            Ddl::CreatedTable => {
                self.pg_refresh(conn, &database);
                self.open_collection(Sel { conn, db: database, schema, coll: table }, false);
                if let Some(t) = self.tm() {
                    t.view = View::Structure;
                }
            }
            Ddl::DroppedTable => {
                self.remove_tabs(|t| in_schema(t, &schema) && t.sel.coll == table);
                self.pg_refresh(conn, &database);
            }
            Ddl::Emptied => {}
        }
    }

    pub(super) fn pg_new_db_dialog(&mut self, ui: &mut egui::Ui, f: &mut PgNewDb, keep: &mut bool) {
        ui.set_width(540.0);
        let writable = self.conn_writable(f.conn);
        dialog_title(ui, "New database");
        ui.label(RichText::new(self.conn_name(f.conn)).color(MUTED()));
        ui.add_space(8.0);
        egui::Grid::new("pg-newdb").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, |ui| {
            form_label(ui, "Name");
            let r = editor_label(ui, &mut f.name, 400.0, "Database name");
            if !f.focused {
                r.request_focus();
                f.focused = true;
            }
            ui.end_row();
            if let Some(t) = &f.templates {
                form_label(ui, "Template");
                let shown = f.template.clone().unwrap_or_else(|| "Server default (template1)".into());
                pick(ui, "pg-newdb-template", 400.0, &shown, |ui| {
                    if ui.selectable_label(f.template.is_none(), "Server default (template1)").clicked() {
                        f.template = None;
                    }
                    for k in t {
                        if ui.selectable_label(f.template.as_deref() == Some(k.as_str()), k).clicked() {
                            f.template = Some(k.clone());
                        }
                    }
                });
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        let templates = f.templates.clone().unwrap_or_default();
        let built = create_pg::create_db_sql(&f.name, f.template.as_deref(), &templates);
        match (&built, &f.load_err) {
            (_, Some(e)) => {
                ui.label(RichText::new(format!("Could not read the server's templates: {e}")).size(TXT_SMALL).color(ALERT()));
            }
            (Ok(sql), _) => sql_block(ui, &format!("{sql};")),
            (Err(e), _) => {
                sql_block(ui, "CREATE DATABASE \"name\";");
                // An empty name is a form not filled in yet, not a mistake to point at.
                if !f.name.is_empty() {
                    ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
                }
            }
        }
        ui.label(RichText::new("The owner is the role this connection signs in as. A database cannot be locked, so undo removes it only while it is empty and nobody else is connected.").size(TXT_SMALL).color(FAINT()));
        if !writable {
            ui.label(RichText::new(NOTE_DB).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && f.templates.is_some() && built.is_ok();
        right_actions(ui, |ui| {
            if ui.add_enabled(ok, primary_button(ui, "Create", PRIMARY())).clicked() {
                if let Some(sql) = self.sql_db_for(f.conn, None) {
                    self.worker.pg_create_db(f.conn, sql, f.name.clone(), f.template.clone(), !writable);
                }
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    pub(super) fn pg_new_schema_dialog(&mut self, ui: &mut egui::Ui, f: &mut PgNewSchema, keep: &mut bool) {
        ui.set_width(480.0);
        let writable = self.conn_writable(f.conn);
        dialog_title(ui, "New schema");
        ui.label(RichText::new(format!("{} · {}", self.conn_name(f.conn), f.database)).color(MUTED()));
        ui.add_space(8.0);
        egui::Grid::new("pg-newschema").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, |ui| {
            form_label(ui, "Name");
            let r = editor_label(ui, &mut f.name, 320.0, "Schema name");
            if !f.focused {
                r.request_focus();
                f.focused = true;
            }
            ui.end_row();
        });
        ui.add_space(8.0);
        let built = create_pg::create_schema_sql(&f.name);
        match &built {
            Ok(sql) => sql_block(ui, &format!("{sql};")),
            Err(e) => {
                sql_block(ui, "CREATE SCHEMA \"name\";");
                if !f.name.is_empty() {
                    ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
                }
            }
        }
        if !writable {
            ui.label(RichText::new(NOTE_DB).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && built.is_ok();
        right_actions(ui, |ui| {
            if ui.add_enabled(ok, primary_button(ui, "Create schema", PRIMARY())).clicked() {
                if let Some(sql) = self.sql_db_for(f.conn, None) {
                    self.worker.pg_create_schema(f.conn, sql.at(&f.database), f.name.clone(), !writable);
                }
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    pub(super) fn pg_new_table_dialog(&mut self, ui: &mut egui::Ui, f: &mut PgNewTable, keep: &mut bool) {
        ui.set_max_width(780.0);
        ui.set_width(740.0);
        let writable = self.conn_writable(f.conn);
        dialog_title(ui, "New table");
        ui.label(RichText::new(format!("{} · {}", f.database, f.schema)).color(MUTED()));
        ui.add_space(8.0);
        egui::Grid::new("pg-newtable-head").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Name");
            let r = editor_label(ui, &mut f.name, 320.0, "Table name");
            if !f.focused {
                r.request_focus();
                f.focused = true;
            }
            ui.end_row();
            form_label(ui, "Comment");
            ui.add(egui::TextEdit::singleline(&mut f.comment).desired_width(320.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Table comment"));
            ui.end_row();
        });
        ui.add_space(8.0);
        self.pg_column_grid(ui, f);
        ui.add_space(4.0);
        let built = create_pg::create_table_stmts(&f.spec());
        ui.horizontal(|ui| {
            if add_chip(ui, "Column").clicked() {
                let n = f.cols.len() + 1;
                f.cols.push(PgCol { def: ColDef::new(&format!("column_{n}"), "text"), pk: false, unique: false });
            }
            ui.label(RichText::new("Columns, keys and indexes can be changed later in Structure").size(TXT_SMALL).color(FAINT()));
        });
        if f.show_sql {
            ui.add_space(6.0);
            match &built {
                Ok(stmts) => sql_block(ui, &stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")),
                Err(_) => {
                    ui.label(RichText::new("The statement appears here once the form is valid.").size(TXT_SMALL).color(FAINT()));
                }
            }
        }
        if let Err(e) = &built {
            if !f.name.is_empty() {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
            }
        }
        if !writable {
            ui.label(RichText::new(NOTE_TABLE).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && built.is_ok();
        right_actions(ui, |ui| {
            if ui.add_enabled(ok, primary_button(ui, "Create table", PRIMARY())).clicked() {
                if let Some(sql) = self.sql_db_for(f.conn, None) {
                    self.worker.pg_create_table(f.conn, sql.at(&f.database), f.spec(), !writable);
                }
                *keep = false;
            }
            if ui.button("Preview SQL").clicked() {
                f.show_sql = !f.show_sql;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    /// The editable column list: key mark and name, type, Null, Unique and an Extra menu with identity, default and comment.
    fn pg_column_grid(&mut self, ui: &mut egui::Ui, f: &mut PgNewTable) {
        let mut remove = None;
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(250.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("pg-newtable-cols").num_columns(7).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("");
                    for h in ["COLUMN", "TYPE", "NULL", "UNIQUE", "EXTRA", ""] {
                        caption(ui, h);
                    }
                    ui.end_row();
                    let n = f.cols.len();
                    for (i, c) in f.cols.iter_mut().enumerate() {
                        ui.label(RichText::new((i + 1).to_string()).monospace().size(TXT_CAPTION).color(FAINT()));
                        ui.horizontal(|ui| {
                            let pk = ui.add(quiet_button(RichText::new("PK").monospace().size(TXT_CAPTION).color(if c.pk { WARNING() } else { FAINT() }))).on_hover_text("Part of the primary key");
                            if pk.clicked() {
                                c.pk = !c.pk;
                            }
                            ui.add_sized([150.0, 24.0], egui::TextEdit::singleline(&mut c.def.name).font(egui::TextStyle::Monospace)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Column name {}", i + 1)));
                        });
                        ui.horizontal(|ui| {
                            ui.add_sized([150.0, 24.0], egui::TextEdit::singleline(&mut c.def.ty).font(egui::TextStyle::Monospace).hint_text("integer")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Column type {}", i + 1)));
                            egui::ComboBox::from_id_salt(("pg-newtable-ty", i)).width(16.0).selected_text("").show_ui(ui, |ui| {
                                menu_look(ui);
                                for t in TYPES {
                                    if ui.selectable_label(c.def.ty == t, t).clicked() {
                                        c.def.ty = t.to_string();
                                    }
                                }
                            });
                        });
                        // A key or identity column is never null, so the box shows that instead of letting it be set.
                        let forced = c.pk || c.def.auto_increment;
                        let mut shown = c.def.nullable && !forced;
                        if ui.add_enabled(!forced, egui::Checkbox::without_text(&mut shown)).changed() {
                            c.def.nullable = shown;
                        }
                        ui.checkbox(&mut c.unique, "").widget_info(|| egui::WidgetInfo::labeled(egui::Role::CheckBox, true, format!("Unique {}", i + 1)));
                        sticky_menu(ui, extra_summary(c), |ui| {
                            ui.set_min_width(220.0);
                            caption(ui, "IDENTITY");
                            let now = if !c.def.auto_increment { 0 } else if c.def.identity_always { 2 } else { 1 };
                            for (v, label) in [(0, "None"), (1, "Generated by default"), (2, "Generated always")] {
                                if ui.selectable_label(now == v, label).clicked() {
                                    c.def.auto_increment = v > 0;
                                    c.def.identity_always = v == 2;
                                    if v > 0 {
                                        c.def.default = Dv::None;
                                        c.def.nullable = false;
                                    }
                                }
                            }
                            ui.add_space(2.0);
                            caption(ui, "DEFAULT");
                            ui.add_enabled_ui(!c.def.auto_increment, |ui| {
                                if ui.selectable_label(matches!(c.def.default, Dv::None | Dv::Null), "None").clicked() {
                                    c.def.default = Dv::None;
                                }
                                if ui.selectable_label(matches!(c.def.default, Dv::Value(_)), "Value").clicked() && !matches!(c.def.default, Dv::Value(_)) {
                                    c.def.default = Dv::Value(String::new());
                                }
                                if ui.selectable_label(matches!(c.def.default, Dv::Expr(_)), "Expression").clicked() && !matches!(c.def.default, Dv::Expr(_)) {
                                    c.def.default = Dv::Expr(String::new());
                                }
                                match &mut c.def.default {
                                    Dv::Value(t) => {
                                        ui.add(egui::TextEdit::singleline(t).font(egui::TextStyle::Monospace).desired_width(200.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Column default {}", i + 1)));
                                    }
                                    Dv::Expr(t) => {
                                        ui.add(egui::TextEdit::singleline(t).font(egui::TextStyle::Monospace).desired_width(200.0).hint_text("now()")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Column default {}", i + 1)));
                                    }
                                    _ => {}
                                }
                            });
                            ui.add_space(2.0);
                            caption(ui, "COMMENT");
                            ui.add(egui::TextEdit::singleline(&mut c.def.comment).desired_width(200.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Column comment {}", i + 1)));
                        });
                        if n > 1 && ui.add(quiet_button(RichText::new("remove").size(TXT_CAPTION).color(FAINT()))).on_hover_text("Remove this column").clicked() {
                            remove = Some(i);
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            });
        });
        if let Some(i) = remove {
            f.cols.remove(i);
        }
    }

    /// The body of a PostgreSQL drop confirmation: what goes, what a drop cannot do, and the opt-in that widens it (CASCADE or
    /// WITH (FORCE)), which is ticked in this dialog and never remembered. True when the drop cannot run as things stand.
    pub(super) fn pg_drop_notes(&mut self, ui: &mut egui::Ui, f: &mut DropForm, info: &DropInfo, name: &str, conn: &str) -> bool {
        let wrap = |ui: &mut egui::Ui, text: String, color: Color32| {
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        };
        let mut blocked = false;
        match f.kind {
            DropKind::Database => {
                let size = info.size.map(|b| format!(" ({})", human_bytes(b))).unwrap_or_default();
                wrap(ui, format!("This deletes the database {name} on {conn} with everything in it{size}. It cannot be undone, and a dropped database has no undo in Quarry either."), TEXT());
                if info.own_database {
                    blocked = true;
                    wrap(ui, format!("{conn} opens on {name}, and Quarry keeps its own connection there, so it cannot be dropped from this connection. Connect with another database in the address (postgres, say), then drop it."), WARNING());
                }
                if info.is_template {
                    blocked = true;
                    wrap(ui, format!("{name} is marked as a template database, and PostgreSQL refuses to drop one until it is no longer a template."), WARNING());
                }
                wrap(ui, "Its open tabs here are closed and Quarry's own connections to it are closed first.".into(), MUTED());
                if let Some(n) = info.sessions.filter(|n| *n > 0) {
                    wrap(ui, format!("{} connected to it right now (this app's own among them, if any). PostgreSQL refuses the drop while anyone is connected.", plural(n, "session")), MUTED());
                }
                ui.add_space(4.0);
                ui.checkbox(&mut f.opt, "Disconnect the other sessions (WITH (FORCE))").on_hover_text("They lose their work in progress. Needs the right to end their sessions.");
            }
            DropKind::Schema | DropKind::Table | DropKind::View => {
                let what = match f.kind {
                    DropKind::Schema => format!("the schema {name} in {}", f.database),
                    DropKind::View => format!("the view {name}"),
                    _ => format!("the table {name}"),
                };
                let rows = match (f.kind, info.rows, info.rows_estimated) {
                    (DropKind::Table, Some(n), false) => format!(", with {}", plural(n, "row")),
                    (DropKind::Table, Some(n), true) if n > 0 => format!(", with about {}", plural(n, "row")),
                    _ => String::new(),
                };
                wrap(ui, format!("This deletes {what} on {conn}{rows}. It cannot be undone, and a drop has no undo in Quarry."), TEXT());
                if info.dependents_total > 0 {
                    let more = info.dependents_total.saturating_sub(info.dependents.len() as u64);
                    let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
                    let lead = if f.kind == DropKind::Schema { format!("It holds {}", plural(info.dependents_total, "object")) } else { format!("{} depend on it", plural(info.dependents_total, "object")) };
                    wrap(ui, format!("{lead}: {}{tail}.", list_and(&info.dependents)), MUTED());
                    ui.add_space(4.0);
                    ui.checkbox(&mut f.opt, if f.kind == DropKind::Schema { "Drop them too (CASCADE)" } else { "Drop what depends on it too (CASCADE)" });
                    if !f.opt {
                        blocked = true;
                        wrap(ui, "PostgreSQL refuses the drop while these exist. Tick CASCADE to drop them with it.".into(), MUTED());
                    } else {
                        wrap(ui, "CASCADE drops every object listed, and their rows, for good.".into(), WARNING());
                    }
                }
            }
            DropKind::Truncate => {}
        }
        blocked
    }
}

fn extra_summary(c: &PgCol) -> String {
    let mut parts = Vec::new();
    if c.def.auto_increment {
        parts.push(if c.def.identity_always { "IDENTITY ALWAYS".to_string() } else { "IDENTITY".to_string() });
    }
    match &c.def.default {
        Dv::Value(t) | Dv::Expr(t) => parts.push(format!("DEFAULT {t}")),
        _ => {}
    }
    if !c.def.comment.is_empty() {
        parts.push("COMMENT".into());
    }
    if parts.is_empty() { "options".into() } else { parts.join(" ") }
}

fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (b as f64, 0);
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_column_is_an_identity_key() {
        let t = PgNewTable { conn: 1, database: "d".into(), schema: "public".into(), name: "t".into(), cols: vec![id_col()], comment: String::new(), show_sql: false, focused: false };
        let s = create_pg::create_table_stmts(&t.spec()).unwrap();
        assert_eq!(s[0], "CREATE TABLE \"public\".\"t\" (\n  \"id\" integer GENERATED BY DEFAULT AS IDENTITY,\n  PRIMARY KEY (\"id\")\n)");
    }

    #[test]
    fn sizes_read_in_the_largest_unit() {
        assert_eq!(human_bytes(900), "900 B");
        assert_eq!(human_bytes(8 * 1024 * 1024 + 300_000), "8.3 MB");
    }
}
