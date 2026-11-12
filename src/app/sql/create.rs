//! Creating and dropping SQL databases and tables: the New database and New table dialogs with their
//! statement previews, and the typed confirmations for drops. The statements come from `db::sql::create`.

use crate::app::*;
use db::sql::create::{self, ColumnSpec, DefaultKind, Ddl, DropInfo, Meta, TableSpec, TYPES};

pub(in crate::app) const NOTE_DB: &str = "Read-only: unlock writes to create a database";
pub(in crate::app) const NOTE_TABLE: &str = "Read-only: unlock writes to create a table";
pub(in crate::app) const NOTE_DROP: &str = "Read-only: unlock writes to drop or empty";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum DropKind {
    Database,
    Table,
    View,
    /// Delete every row and keep the table.
    Truncate,
    /// A PostgreSQL schema.
    Schema,
}

pub(in crate::app) struct NewDb {
    pub(in crate::app) conn: u64,
    pub(in crate::app) name: String,
    pub(in crate::app) charset: String,
    pub(in crate::app) collation: String,
    /// None while the server's lists load.
    pub(in crate::app) meta: Option<Meta>,
    pub(in crate::app) load_err: Option<String>,
}

pub(in crate::app) struct NewTable {
    pub(in crate::app) conn: u64,
    pub(in crate::app) schema: String,
    pub(in crate::app) name: String,
    pub(in crate::app) engine: String,
    pub(in crate::app) collation: String,
    pub(in crate::app) cols: Vec<ColumnSpec>,
    pub(in crate::app) meta: Option<Meta>,
    pub(in crate::app) load_err: Option<String>,
    pub(in crate::app) show_sql: bool,
}

pub(in crate::app) struct DropForm {
    pub(in crate::app) conn: u64,
    /// PostgreSQL: the database the schema or table is in, or the database itself for a database drop. Empty on MySQL.
    pub(in crate::app) database: String,
    pub(in crate::app) schema: String,
    /// Empty for a database.
    pub(in crate::app) table: String,
    pub(in crate::app) kind: DropKind,
    pub(in crate::app) typed: String,
    /// PostgreSQL: CASCADE for a schema or table, WITH (FORCE) for a database. Ticked inside the dialog, never by default.
    pub(in crate::app) opt: bool,
    /// None while the counts load.
    pub(in crate::app) info: Option<Result<DropInfo, String>>,
}

pub(in crate::app) fn plural(n: u64, word: &str) -> String {
    format!("{} {word}{}", group(&n.to_string()), if n == 1 { "" } else { "s" })
}

/// "a, b and c".
pub(in crate::app) fn list_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

pub(in crate::app) fn pick(ui: &mut egui::Ui, id: &str, width: f32, shown: &str, items: impl FnOnce(&mut egui::Ui)) {
    egui::ComboBox::from_id_salt(id).width(width).selected_text(shown.to_string()).show_ui(ui, |ui| {
        menu_look(ui);
        items(ui);
    });
}

/// The preview block of the design: the statement in mono on the page color.
pub(in crate::app) fn sql_block(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(sql_job(text, TXT_BODY)).selectable(true));
    });
}

/// The row of buttons at the bottom, right aligned as in the designs: the main action first in the
/// code so it ends up rightmost.
pub(in crate::app) fn right_actions(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
}

impl App {
    pub(in crate::app) fn conn_name(&self, conn: u64) -> String {
        self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default()
    }

    pub(in crate::app) fn conn_writable(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.writable)
    }

    /// Asks the server for what the dialogs list. The offline demo has no server, so its dialogs are filled directly.
    fn ask_meta(&mut self, conn: u64, schema: &str) {
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_create_meta(conn, sql, schema.to_string());
        }
    }

    /// A create or drop on a locked connection asks to allow writes first, then goes on with what was asked.
    pub(in crate::app) fn ask_unlock_on(&mut self, conn: u64, then: impl FnOnce(&mut App) + 'static) {
        self.modal = Some(Modal::Unlock(conn, Some(Box::new(then))));
    }

    pub(in crate::app) fn sql_new_db(&mut self, conn: u64) {
        if self.is_pg(conn) {
            return self.pg_new_db(conn);
        }
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.sql_new_db(conn));
        }
        self.ask_meta(conn, "");
        self.modal = Some(Modal::SqlNewDb(Box::new(NewDb { conn, name: String::new(), charset: String::new(), collation: String::new(), meta: None, load_err: None })));
    }

    pub(in crate::app) fn sql_new_table(&mut self, conn: u64, schema: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.sql_new_table(conn, schema));
        }
        self.ask_meta(conn, &schema);
        self.modal = Some(Modal::SqlNewTable(Box::new(NewTable { conn, schema, name: String::new(), engine: String::new(), collation: String::new(), cols: vec![ColumnSpec::id()], meta: None, load_err: None, show_sql: false })));
    }

    pub(in crate::app) fn sql_drop_ask(&mut self, conn: u64, schema: String, table: String, kind: DropKind) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.sql_drop_ask(conn, schema, table, kind));
        }
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_drop_info(conn, sql, schema.clone(), table.clone());
        }
        self.modal = Some(Modal::SqlDrop(Box::new(DropForm { conn, database: String::new(), schema, table, kind, typed: String::new(), opt: false, info: None })));
    }

    /// The lists arrived: fill the open dialog and pick the server's defaults.
    pub(in crate::app) fn sql_meta_done(&mut self, conn: u64, schema: String, res: db::Res<Meta>) {
        match &mut self.modal {
            Some(Modal::SqlNewDb(f)) if f.conn == conn && schema.is_empty() => match res {
                Ok(m) => {
                    f.charset = m.server_charset.clone();
                    f.collation = if m.charset_of(&m.server_collation) == Some(f.charset.as_str()) { m.server_collation.clone() } else { m.default_collation(&f.charset).unwrap_or_default().to_string() };
                    f.meta = Some(m);
                }
                Err(e) => f.load_err = Some(e),
            },
            Some(Modal::SqlNewTable(f)) if f.conn == conn && f.schema == schema => match res {
                Ok(m) => {
                    f.engine = m.engines.iter().find(|e| e.eq_ignore_ascii_case(&m.server_engine)).or(m.engines.first()).cloned().unwrap_or_default();
                    f.collation = m.starting_collation(m.schema_collation.as_deref());
                    f.meta = Some(m);
                }
                Err(e) => f.load_err = Some(e),
            },
            _ => {}
        }
    }

    pub(in crate::app) fn sql_drop_info_done(&mut self, conn: u64, database: String, schema: String, table: String, res: db::Res<DropInfo>) {
        if let Some(Modal::SqlDrop(f)) = &mut self.modal {
            if f.conn == conn && f.database == database && f.schema == schema && f.table == table {
                f.info = Some(res);
            }
        }
    }

    /// A create or drop finished: refresh what the sidebar and the tabs show of it.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn sql_ddl_done(&mut self, conn: u64, database: String, schema: String, table: String, what: Ddl, res: db::Res<String>, undo: Option<UndoAction>, dbs: Option<Vec<String>>, sql_mode: Option<String>) {
        // The create was built for the session's mode, which changed since connecting; the previews follow it now.
        self.adopt_sql_mode(conn, sql_mode);
        // A refused undo hands its entry back, so it is kept whether or not the write went through.
        if let Some(u) = undo {
            self.push_undo(u);
        }
        if self.is_pg(conn) {
            return self.pg_ddl_done(conn, database, schema, table, what, res, dbs);
        }
        match res {
            Ok(s) => self.ok(s),
            Err(e) => return self.err(e),
        }
        if let (Some(dbs), Some(c)) = (dbs, self.conns.iter_mut().find(|c| c.meta.id == conn)) {
            let old = std::mem::take(&mut c.dbs);
            c.dbs = dbs.into_iter().map(|d| (d.clone(), old.iter().find(|o| o.0 == d).and_then(|o| o.1.clone()))).collect();
            // A database just created is known to be empty, so it shows 0 instead of a spinner.
            if what == Ddl::CreatedDb {
                if let Some(d) = c.dbs.iter_mut().find(|d| d.0 == schema) {
                    d.1 = Some(Vec::new());
                }
            }
        }
        let gone = |t: &Tab| t.sel.conn == conn && t.sel.db == schema && (table.is_empty() || t.sel.coll == table);
        match what {
            Ddl::CreatedDb => {
                self.sql_asked.insert((conn, schema.clone()));
                self.open_dbs.insert((conn, schema.clone()));
                self.reveal = Some((conn, schema, None));
            }
            Ddl::CreatedTable => {
                self.side_act(conn, SideAct::Refresh(schema.clone()));
                self.open_collection(Sel { conn, db: schema, coll: table, ..Default::default() }, false);
                if let Some(t) = self.tm() {
                    t.view = View::Structure;
                }
            }
            Ddl::DroppedDb => {
                self.purge_undo(conn, "", &schema, "");
                self.remove_tabs(gone);
                self.open_dbs.remove(&(conn, schema.clone()));
                self.sql_asked.remove(&(conn, schema.clone()));
                self.forget_schema(conn, &schema);
            }
            Ddl::DroppedTable => {
                self.purge_undo(conn, "", &schema, &table);
                self.remove_tabs(gone);
                self.side_act(conn, SideAct::Refresh(schema));
            }
            Ddl::CreatedSchema | Ddl::DroppedSchema => {}
            Ddl::Emptied => {
                let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| gone(&self.tabs[i])).collect();
                for i in open {
                    self.reload_of(i);
                }
                self.side_act(conn, SideAct::Refresh(schema));
            }
        }
    }

    /// Runs a confirmed drop. Tabs close and the sidebar updates when the server says it worked.
    pub(in crate::app) fn sql_drop_run(&mut self, conn: u64, database: String, schema: String, table: String, kind: DropKind, opt: bool) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return self.warn_offline() };
        if c.meta.engine == db::sql::Engine::Postgres {
            return self.pg_drop_run(conn, sql, database, schema, table, kind, opt);
        }
        self.worker.sql_drop(conn, sql, schema, table, kind == DropKind::View, kind == DropKind::Truncate, false);
    }

    pub(in crate::app) fn sql_new_db_dialog(&mut self, ui: &mut egui::Ui, f: &mut NewDb, keep: &mut bool) {
        ui.set_width(540.0);
        let writable = self.conn_writable(f.conn);
        dialog_title(ui, "New database");
        ui.label(RichText::new(self.conn_name(f.conn)).color(MUTED()));
        ui.add_space(8.0);
        let d = self.conn(f.conn).and_then(|c| c.sql.as_ref().map(|s| s.dialect)).unwrap_or_default();
        egui::Grid::new("newdb").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, |ui| {
            form_label(ui, "Name");
            let r = ui.add(egui::TextEdit::singleline(&mut f.name).font(egui::TextStyle::Monospace).desired_width(400.0));
            if f.meta.is_some() && !r.has_focus() && f.name.is_empty() {
                r.request_focus();
            }
            ui.end_row();
            if let Some(m) = &f.meta {
                form_label(ui, "Charset");
                let shown = m.charsets.iter().find(|c| c.name == f.charset).map_or(f.charset.clone(), |c| if c.desc.is_empty() { c.name.clone() } else { format!("{} · {}", c.name, c.desc) });
                pick(ui, "newdb-cs", 400.0, &shown, |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for c in &m.charsets {
                            if ui.selectable_label(c.name == f.charset, if c.desc.is_empty() { c.name.clone() } else { format!("{} · {}", c.name, c.desc) }).clicked() {
                                f.charset = c.name.clone();
                                f.collation = c.default_collation.clone();
                            }
                        }
                    });
                });
                ui.end_row();
                form_label(ui, "Collation");
                let label = |c: &create::Collation| if c.is_default { format!("{} (default)", c.name) } else { c.name.clone() };
                let shown = m.collations.iter().find(|c| c.name == f.collation).map_or(f.collation.clone(), label);
                pick(ui, "newdb-co", 400.0, &shown, |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for c in m.collations_of(&f.charset) {
                            if ui.selectable_label(c.name == f.collation, label(c)).clicked() {
                                f.collation = c.name.clone();
                            }
                        }
                    });
                });
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        let built = match &f.meta {
            Some(m) => Some(create::create_db_sql(d, &f.name, &f.charset, &f.collation, m)),
            None => None,
        };
        match (&built, &f.load_err) {
            (_, Some(e)) => {
                ui.label(RichText::new(format!("Could not read the server's charsets: {e}")).size(TXT_SMALL).color(ALERT()));
            }
            (None, None) => {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Reading the server's charsets…").color(MUTED()));
                });
            }
            (Some(Ok(sql)), _) => sql_block(ui, &create::shown(sql)),
            (Some(Err(e)), _) => {
                // An empty name is a form not filled in yet, not a mistake to point at.
                sql_block(ui, "CREATE DATABASE `name`\n  CHARACTER SET … COLLATE …;");
                if !f.name.is_empty() {
                    ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
                }
            }
        }
        if !writable {
            ui.label(RichText::new(NOTE_DB).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && matches!(built, Some(Ok(_)));
        right_actions(ui, |ui| {
            if ui.add_enabled(ok, primary_button(ui, "Create", PRIMARY())).clicked() {
                self.sql_create_db_run(f);
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    fn sql_create_db_run(&mut self, f: &NewDb) {
        let Some(sql) = self.sql_db_for(f.conn, None) else { return };
        let Some(meta) = f.meta.clone() else { return };
        self.worker.sql_create_db(f.conn, sql, f.name.clone(), f.charset.clone(), f.collation.clone(), meta, !self.conn_writable(f.conn));
    }

    pub(in crate::app) fn sql_new_table_dialog(&mut self, ui: &mut egui::Ui, f: &mut NewTable, keep: &mut bool) {
        ui.set_max_width(740.0);
        ui.set_width(700.0);
        let writable = self.conn_writable(f.conn);
        dialog_title(ui, "New table");
        ui.label(RichText::new(f.schema.as_str()).color(MUTED()));
        ui.add_space(8.0);
        let d = self.conn(f.conn).and_then(|c| c.sql.as_ref().map(|s| s.dialect)).unwrap_or_default();
        let bs = self.sql_mode(f.conn).bs;
        egui::Grid::new("newtable-head").num_columns(4).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Name");
            let r = ui.add(egui::TextEdit::singleline(&mut f.name).font(egui::TextStyle::Monospace).desired_width(280.0));
            if f.meta.is_some() && !r.has_focus() && f.name.is_empty() {
                r.request_focus();
            }
            form_label(ui, "Engine");
            match &f.meta {
                Some(m) => pick(ui, "newtable-engine", 130.0, &f.engine.clone(), |ui| {
                    for e in &m.engines {
                        if ui.selectable_label(*e == f.engine, e).clicked() {
                            f.engine = e.clone();
                        }
                    }
                }),
                None => {
                    spin(ui);
                }
            }
            ui.end_row();
            if let Some(m) = &f.meta {
                form_label(ui, "Collation");
                let from = if m.schema_collation.as_deref() == Some(f.collation.as_str()) { format!(" (from {})", f.schema) } else if f.collation == m.server_collation { " (server default)".to_string() } else { String::new() };
                pick(ui, "newtable-co", 280.0, &format!("{}{from}", f.collation), |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for c in &m.collations {
                            if ui.selectable_label(c.name == f.collation, format!("{}  ({})", c.name, c.charset)).clicked() {
                                f.collation = c.name.clone();
                            }
                        }
                    });
                });
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        self.column_grid(ui, f);
        ui.add_space(4.0);
        let built = f.meta.as_ref().map(|m| create::create_table_sql(d, bs, &TableSpec { schema: f.schema.clone(), name: f.name.clone(), engine: f.engine.clone(), collation: f.collation.clone(), columns: f.cols.clone() }, m));
        ui.horizontal(|ui| {
            if add_chip(ui, "Column").clicked() {
                let n = f.cols.len() + 1;
                f.cols.push(ColumnSpec { name: format!("column_{n}"), ..Default::default() });
            }
            ui.label(RichText::new("Columns can be changed later in Structure").size(TXT_SMALL).color(FAINT()));
        });
        if let Some(e) = &f.load_err {
            ui.label(RichText::new(format!("Could not read the server's engines and collations: {e}")).size(TXT_SMALL).color(ALERT()));
        }
        if f.show_sql {
            ui.add_space(6.0);
            match &built {
                Some(Ok(sql)) => sql_block(ui, &create::shown(sql)),
                _ => {
                    ui.label(RichText::new("The statement appears here once the form is valid.").size(TXT_SMALL).color(FAINT()));
                }
            }
        }
        if let Some(Err(e)) = &built {
            if !f.name.is_empty() {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
            }
        }
        if !writable {
            ui.label(RichText::new(NOTE_TABLE).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && matches!(built, Some(Ok(_)));
        right_actions(ui, |ui| {
            if ui.add_enabled(ok, primary_button(ui, "Create table", PRIMARY())).clicked() {
                self.sql_create_table_run(f);
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

    fn sql_create_table_run(&mut self, f: &NewTable) {
        let Some(sql) = self.sql_db_for(f.conn, None) else { return };
        let Some(meta) = f.meta.clone() else { return };
        let spec = TableSpec { schema: f.schema.clone(), name: f.name.clone(), engine: f.engine.clone(), collation: f.collation.clone(), columns: f.cols.clone() };
        self.worker.sql_create_table(f.conn, sql, spec, meta, !self.conn_writable(f.conn));
    }

    /// The editable column list: name with a key toggle, type with its length, Null, and an Extra menu.
    fn column_grid(&mut self, ui: &mut egui::Ui, f: &mut NewTable) {
        let mut remove = None;
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(250.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("newtable-cols").num_columns(6).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("");
                    for h in ["COLUMN", "TYPE", "NULL", "EXTRA", ""] {
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
                                if !c.pk {
                                    c.auto = false;
                                }
                            }
                            ui.add_sized([150.0, 24.0], egui::TextEdit::singleline(&mut c.name).font(egui::TextStyle::Monospace));
                        });
                        ui.horizontal(|ui| {
                            let shown = if c.unsigned && create::type_def(&c.ty).is_some_and(|t| t.numeric && t.name != "BOOLEAN") { format!("{} UNSIGNED", c.ty) } else { c.ty.clone() };
                            pick(ui, &format!("newtable-ty-{i}"), 150.0, &shown, |ui| {
                                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                    for t in TYPES {
                                        if ui.selectable_label(c.ty == t.name, t.name).clicked() {
                                            retype(c, t);
                                        }
                                    }
                                });
                            });
                            let shape = create::type_def(&c.ty).map(|t| t.shape);
                            let hint = match shape {
                                Some(create::Shape::Precision) => "10,2",
                                Some(create::Shape::Fsp) => "0-6",
                                _ => "length",
                            };
                            let takes_len = !matches!(shape, Some(create::Shape::Plain) | None);
                            ui.add_enabled_ui(takes_len, |ui| ui.add_sized([64.0, 24.0], egui::TextEdit::singleline(&mut c.len).font(egui::TextStyle::Monospace).hint_text(hint)));
                        });
                        // Key columns are never null, so the box shows that instead of letting it be set.
                        ui.add_enabled(!c.pk, egui::Checkbox::without_text(&mut c.nullable));
                        let def = create::type_def(&c.ty);
                        sticky_menu(ui, extra_summary(c), |ui| {
                            ui.set_min_width(200.0);
                            if def.is_some_and(|t| t.numeric && t.name != "BOOLEAN") {
                                ui.checkbox(&mut c.unsigned, "Unsigned");
                            }
                            if def.is_some_and(|t| t.integer) {
                                ui.add_enabled(c.pk, egui::Checkbox::new(&mut c.auto, "Auto increment")).on_disabled_hover_text("Needs the PK mark");
                            }
                            ui.add_space(2.0);
                            caption(ui, "DEFAULT");
                            ui.add_enabled_ui(!c.auto, |ui| {
                                ui.selectable_value(&mut c.default, DefaultKind::None, "None");
                                ui.add_enabled_ui(c.nullable && !c.pk, |ui| ui.selectable_value(&mut c.default, DefaultKind::Null, "NULL"));
                                if def.is_some_and(|t| t.temporal_now) {
                                    ui.selectable_value(&mut c.default, DefaultKind::Now, "Current time");
                                }
                                if def.is_some_and(|t| !t.no_default) {
                                    ui.selectable_value(&mut c.default, DefaultKind::Value, "Value");
                                    if c.default == DefaultKind::Value {
                                        ui.add(egui::TextEdit::singleline(&mut c.default_text).font(egui::TextStyle::Monospace).desired_width(180.0));
                                    }
                                }
                            });
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

    pub(in crate::app) fn sql_drop_dialog(&mut self, ui: &mut egui::Ui, f: &mut DropForm, keep: &mut bool) {
        ui.set_width(560.0);
        let writable = self.conn_writable(f.conn);
        let conn = self.conn_name(f.conn);
        let tag = self.conn(f.conn).map(|c| c.meta.tag.clone()).unwrap_or_default();
        let uri_db = self.conn(f.conn).and_then(|c| c.sql.as_ref()).and_then(|d| d.opts.database().map(String::from));
        let (name, noun, button) = match f.kind {
            DropKind::Database => (f.schema.as_str(), "database", "Drop database"),
            DropKind::Table => (f.table.as_str(), "table", "Drop table"),
            DropKind::View => (f.table.as_str(), "view", "Drop view"),
            DropKind::Truncate => (f.table.as_str(), "table", "Empty table"),
            DropKind::Schema => (f.schema.as_str(), "schema", "Drop schema"),
        };
        let pg = self.is_pg(f.conn);
        let name = if pg && f.kind == DropKind::Database { f.database.as_str() } else { name }.to_string();
        let name = name.as_str();
        let mut blocked = false;
        // A table or view of PostgreSQL is named with its schema and database, because the same name lives in several.
        let shown = if pg && matches!(f.kind, DropKind::Table | DropKind::View) { format!("{}.{}.{name}", f.database, f.schema) } else { name.to_string() };
        dialog_title(ui, if f.kind == DropKind::Truncate { format!("Empty table {name}?") } else { format!("Drop {noun} {shown}?") });
        let wrap = |ui: &mut egui::Ui, text: String, color: Color32| {
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        };
        let mut loaded = false;
        let shown = f.info.clone();
        match &shown {
            None => {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new(if f.kind == DropKind::Database { "Counting what is in it…" } else { "Counting rows…" }).color(MUTED()));
                });
            }
            Some(Err(e)) => {
                wrap(ui, format!("Could not look up what this holds: {e}"), ALERT());
            }
            Some(Ok(info)) if !info.exists => {
                wrap(ui, format!("The {noun} {name} is not on {conn} any more."), ALERT());
            }
            Some(Ok(info)) if pg => {
                loaded = true;
                blocked = self.pg_drop_notes(ui, f, info, name, &conn);
            }
            Some(Ok(info)) => {
                loaded = true;
                wrap(ui, drop_lead(f.kind, name, &conn, info), TEXT());
                // The connection opens this database by name, so every later connect would fail with 1049.
                if f.kind == DropKind::Database && uri_db.as_deref().is_some_and(|u| u == name || (info.fold_case && u.eq_ignore_ascii_case(name))) {
                    wrap(ui, format!("{conn} opens {name} when it connects (it is the database in its address), so connecting again fails until the address names another database or {name} exists again."), WARNING());
                }
                if matches!(f.kind, DropKind::Table | DropKind::Truncate) && !info.referenced_by.is_empty() {
                    let n = info.referenced_by.len() as u64;
                    let product = self.conn(f.conn).map(|c| c.server.product.clone()).filter(|p| !p.is_empty()).unwrap_or_else(|| "The server".into());
                    let verb = if f.kind == DropKind::Truncate { "to empty it" } else { "the drop" };
                    wrap(ui, format!("{} {} here: {}. {product} refuses {verb} until their foreign keys are removed.", plural(n, "table"), if n == 1 { "points" } else { "point" }, list_and(&info.referenced_by)), MUTED());
                }
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new(format!("Type the {noun} name to confirm")).color(MUTED()));
        ui.add(egui::TextEdit::singleline(&mut f.typed).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Confirmation"));
        if !tag.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                badge(ui, &tag, tag_color(&tag));
                ui.label(RichText::new(format!("This connection is tagged {tag}")).color(tag_color(&tag)));
            });
        }
        if !writable {
            ui.label(RichText::new(NOTE_DROP).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && loaded && !blocked && f.typed == name;
        right_actions(ui, |ui| {
            if ui.add_enabled_ui(ok, |ui| danger(ui, button)).inner.clicked() {
                *keep = false;
                self.danger(Danger::SqlDrop { conn: f.conn, database: f.database.clone(), schema: f.schema.clone(), table: f.table.clone(), kind: f.kind, opt: f.opt });
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    /// QUARRY_TEST_SQL=newdb, newtable or droptable: the dialogs of docs/sql/designs.figcraft, "Create and drop".
    pub(in crate::app) fn sql_create_demo(&mut self, which: &str) {
        if !matches!(which, "newdb" | "newtable" | "droptable") {
            return;
        }
        // The designs show the actions enabled, which they are on an unlocked connection.
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == 3) {
            c.writable = true;
        }
        let cs = |n: &str, d: &str, c: &str| create::Charset { name: n.into(), desc: d.into(), default_collation: c.into() };
        let co = |n: &str, c: &str, d: bool| create::Collation { name: n.into(), charset: c.into(), is_default: d };
        let meta = Meta {
            charsets: vec![cs("latin1", "cp1252 West European", "latin1_swedish_ci"), cs("utf8mb3", "UTF-8 Unicode", "utf8mb3_general_ci"), cs("utf8mb4", "UTF-8 Unicode", "utf8mb4_0900_ai_ci")],
            collations: vec![co("latin1_swedish_ci", "latin1", true), co("utf8mb3_general_ci", "utf8mb3", true), co("utf8mb4_0900_ai_ci", "utf8mb4", true), co("utf8mb4_bin", "utf8mb4", false), co("utf8mb4_unicode_ci", "utf8mb4", false)],
            engines: vec!["InnoDB".into(), "MyISAM".into(), "MEMORY".into()],
            server_charset: "utf8mb4".into(),
            server_collation: "utf8mb4_0900_ai_ci".into(),
            server_engine: "InnoDB".into(),
            schema_collation: Some("utf8mb4_0900_ai_ci".into()),
            fold_case: false,
        };
        self.modal = Some(match which {
            "newdb" => Modal::SqlNewDb(Box::new(NewDb { conn: 3, name: "billing".into(), charset: "utf8mb4".into(), collation: "utf8mb4_0900_ai_ci".into(), meta: Some(meta), load_err: None })),
            "newtable" => {
                let col = |name: &str, ty: &str, len: &str, nullable: bool| ColumnSpec { name: name.into(), ty: ty.into(), len: len.into(), nullable, ..Default::default() };
                let cols = vec![
                    ColumnSpec::id(),
                    ColumnSpec { unsigned: true, ..col("order_id", "INT", "", false) },
                    col("amount", "DECIMAL", "10,2", false),
                    col("reason", "VARCHAR", "255", true),
                    ColumnSpec { default: DefaultKind::Now, ..col("created_at", "DATETIME", "", false) },
                ];
                Modal::SqlNewTable(Box::new(NewTable { conn: 3, schema: "shop".into(), name: "refunds".into(), engine: "InnoDB".into(), collation: "utf8mb4_0900_ai_ci".into(), cols, meta: Some(meta), load_err: None, show_sql: false }))
            }
            _ => Modal::SqlDrop(Box::new(DropForm {
                conn: 3,
                database: String::new(),
                schema: "shop".into(),
                table: "orders".into(),
                kind: DropKind::Table,
                typed: "order".into(),
                opt: false,
                info: Some(Ok(DropInfo { exists: true, rows: Some(4410), referenced_by: vec!["order_items".into(), "payments".into(), "shipments".into()], ..Default::default() })),
            })),
        });
    }
}

/// The rows a drop takes as the dialog says them: an exact count as it is, an estimate as "about N". An estimate
/// of 0 is None like a missing one: the server's statistics lag behind, so it does not mean the table is empty.
fn rows_said(info: &DropInfo) -> Option<String> {
    match (info.rows, info.rows_estimated) {
        (Some(n), false) => Some(plural(n, "row")),
        (Some(n), true) if n > 0 => Some(format!("about {}", plural(n, "row"))),
        _ => None,
    }
}

/// The first line of a drop confirmation: what goes, with the row count only where it is known.
fn drop_lead(kind: DropKind, name: &str, conn: &str, info: &DropInfo) -> String {
    let exact_empty = info.rows == Some(0) && !info.rows_estimated;
    match kind {
        DropKind::Database => {
            let mut parts = vec![plural(info.tables, "table")];
            for (n, w) in [(info.views, "view"), (info.triggers, "trigger"), (info.routines, "routine"), (info.events, "event")] {
                if n > 0 {
                    parts.push(plural(n, w));
                }
            }
            let rows = rows_said(info).filter(|_| !exact_empty).map(|r| format!(", {r}")).unwrap_or_default();
            format!("This deletes the database {name} on {conn} with {}{rows}. Everything in it goes. It cannot be undone.", list_and(&parts))
        }
        DropKind::Table if exact_empty => format!("This deletes the empty table and its structure on {conn}. It cannot be undone."),
        DropKind::Table => match rows_said(info) {
            Some(r) => format!("This deletes {r} and the table's structure on {conn}. It cannot be undone."),
            None => format!("This deletes the table, its rows (row count unknown) and its structure on {conn}. It cannot be undone."),
        },
        DropKind::View => format!("This deletes the view on {conn}. The tables it reads from are not touched. It cannot be undone."),
        DropKind::Schema => String::new(),
        DropKind::Truncate => match rows_said(info) {
            Some(r) if !info.rows_estimated => format!("This deletes all {r} of {name} on {conn}. The table and its structure stay and the auto-increment counter restarts. It cannot be undone."),
            Some(r) => format!("This deletes every row of {name} ({r}) on {conn}. The table and its structure stay and the auto-increment counter restarts. It cannot be undone."),
            None => format!("This deletes every row of {name} on {conn} (row count unknown). The table and its structure stay. It cannot be undone."),
        },
    }
}

/// A new type resets what no longer applies, so the statement never carries a stale length or flag.
fn retype(c: &mut ColumnSpec, t: &create::TypeDef) {
    c.ty = t.name.to_string();
    c.len = match t.name {
        "VARCHAR" | "VARBINARY" => "255".into(),
        "DECIMAL" => "10,2".into(),
        _ => String::new(),
    };
    if !t.numeric {
        c.unsigned = false;
    }
    if !t.integer {
        c.auto = false;
    }
    if (c.default == DefaultKind::Now && !t.temporal_now) || (c.default == DefaultKind::Value && t.no_default) {
        c.default = DefaultKind::None;
    }
}

/// What the Extra cell says, in the design's capitals.
fn extra_summary(c: &ColumnSpec) -> String {
    let mut parts = Vec::new();
    if c.auto {
        parts.push("AUTO_INCREMENT".to_string());
    }
    match c.default {
        DefaultKind::None => {}
        DefaultKind::Null => parts.push("DEFAULT NULL".into()),
        DefaultKind::Now => parts.push("DEFAULT CURRENT_TIMESTAMP".into()),
        DefaultKind::Value => parts.push(format!("DEFAULT {}", c.default_text)),
    }
    if parts.is_empty() { "options".into() } else { parts.join(" ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(rows: Option<u64>, estimated: bool) -> DropInfo {
        DropInfo { exists: true, rows, rows_estimated: estimated, ..Default::default() }
    }

    /// Review finding 2: an estimate of 0 (statistics cached before the table filled) was shown as "the empty table".
    #[test]
    fn an_estimate_is_never_called_empty() {
        for kind in [DropKind::Table, DropKind::Truncate, DropKind::Database] {
            for i in [info(Some(0), true), info(None, true), info(None, false)] {
                let s = drop_lead(kind, "orders", "shop-prod", &i);
                assert!(!s.contains("empty") && !s.contains("about 0") && !s.contains(" 0 rows"), "{s}");
                if kind != DropKind::Database {
                    assert!(s.contains("row count unknown"), "{s}");
                }
            }
            let s = drop_lead(kind, "orders", "shop-prod", &info(Some(1000), true));
            assert!(s.contains("about 1,000 rows"), "{s}");
        }
        assert!(drop_lead(DropKind::Table, "orders", "shop-prod", &info(Some(0), false)).contains("the empty table"));
        assert!(drop_lead(DropKind::Truncate, "orders", "shop-prod", &info(Some(3), false)).contains("all 3 rows of orders"));
        assert!(drop_lead(DropKind::Table, "orders", "shop-prod", &info(Some(1), false)).contains("deletes 1 row and"));
    }
}
