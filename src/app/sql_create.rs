//! Creating and dropping SQL databases and tables: the New database and New table dialogs with their
//! statement previews, and the typed confirmations for drops. The statements come from `db::sql::create`.

use super::*;
use db::sql::create::{self, ColumnSpec, DefaultKind, Ddl, DropInfo, Meta, TableSpec, TYPES};

const NOTE_DB: &str = "Read-only: unlock writes to create a database";
const NOTE_TABLE: &str = "Read-only: unlock writes to create a table";
const NOTE_DROP: &str = "Read-only: unlock writes to drop or empty";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DropKind {
    Database,
    Table,
    View,
    /// Delete every row and keep the table.
    Truncate,
}

pub(super) struct NewDb {
    pub(super) conn: u64,
    pub(super) name: String,
    pub(super) charset: String,
    pub(super) collation: String,
    /// None while the server's lists load.
    pub(super) meta: Option<Meta>,
    pub(super) load_err: Option<String>,
}

pub(super) struct NewTable {
    pub(super) conn: u64,
    pub(super) schema: String,
    pub(super) name: String,
    pub(super) engine: String,
    pub(super) collation: String,
    pub(super) cols: Vec<ColumnSpec>,
    pub(super) meta: Option<Meta>,
    pub(super) load_err: Option<String>,
    pub(super) show_sql: bool,
}

pub(super) struct DropForm {
    pub(super) conn: u64,
    pub(super) schema: String,
    /// Empty for a database.
    pub(super) table: String,
    pub(super) kind: DropKind,
    pub(super) typed: String,
    /// None while the counts load.
    pub(super) info: Option<Result<DropInfo, String>>,
}

fn plural(n: u64, word: &str) -> String {
    format!("{} {word}{}", group(&n.to_string()), if n == 1 { "" } else { "s" })
}

/// "a, b and c".
fn list_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

fn pick(ui: &mut egui::Ui, id: &str, width: f32, shown: &str, items: impl FnOnce(&mut egui::Ui)) {
    egui::ComboBox::from_id_salt(id).width(width).selected_text(shown.to_string()).show_ui(ui, |ui| {
        menu_look(ui);
        items(ui);
    });
}

/// The preview block of the design: the statement in mono on the page color.
fn sql_block(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(sql_job(text, TXT_BODY)).selectable(true));
    });
}

/// The row of buttons at the bottom, right aligned as in the designs: the main action first in the
/// code so it ends up rightmost.
fn right_actions(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
}

impl App {
    fn conn_name(&self, conn: u64) -> String {
        self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default()
    }

    fn conn_writable(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.writable)
    }

    /// Asks the server for what the dialogs list. The offline demo has no server, so its dialogs are filled directly.
    fn ask_meta(&mut self, conn: u64, schema: &str) {
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_create_meta(conn, sql, schema.to_string());
        }
    }

    pub(super) fn sql_new_db(&mut self, conn: u64) {
        if !self.conn_writable(conn) {
            return self.err(NOTE_DB);
        }
        self.ask_meta(conn, "");
        self.modal = Some(Modal::SqlNewDb(Box::new(NewDb { conn, name: String::new(), charset: String::new(), collation: String::new(), meta: None, load_err: None })));
    }

    pub(super) fn sql_new_table(&mut self, conn: u64, schema: String) {
        if !self.conn_writable(conn) {
            return self.err(NOTE_TABLE);
        }
        self.ask_meta(conn, &schema);
        self.modal = Some(Modal::SqlNewTable(Box::new(NewTable { conn, schema, name: String::new(), engine: String::new(), collation: String::new(), cols: vec![ColumnSpec::id()], meta: None, load_err: None, show_sql: false })));
    }

    pub(super) fn sql_drop_ask(&mut self, conn: u64, schema: String, table: String, kind: DropKind) {
        if !self.conn_writable(conn) {
            return self.err(NOTE_DROP);
        }
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_drop_info(conn, sql, schema.clone(), table.clone());
        }
        self.modal = Some(Modal::SqlDrop(Box::new(DropForm { conn, schema, table, kind, typed: String::new(), info: None })));
    }

    /// The lists arrived: fill the open dialog and pick the server's defaults.
    pub(super) fn sql_meta_done(&mut self, conn: u64, schema: String, res: db::Res<Meta>) {
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

    pub(super) fn sql_drop_info_done(&mut self, conn: u64, schema: String, table: String, res: db::Res<DropInfo>) {
        if let Some(Modal::SqlDrop(f)) = &mut self.modal {
            if f.conn == conn && f.schema == schema && f.table == table {
                f.info = Some(res);
            }
        }
    }

    /// A create or drop finished: refresh what the sidebar and the tabs show of it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn sql_ddl_done(&mut self, conn: u64, schema: String, table: String, what: Ddl, res: db::Res<String>, undo: Option<UndoAction>, dbs: Option<Vec<String>>) {
        // A refused undo hands its entry back, so it is kept whether or not the write went through.
        if let Some(u) = undo {
            self.push_undo(u);
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
                self.open_collection(Sel { conn, db: schema, coll: table }, false);
                if let Some(t) = self.tm() {
                    t.view = View::Structure;
                }
            }
            Ddl::DroppedDb => {
                self.tabs.retain(|t| !gone(t));
                self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                self.open_dbs.remove(&(conn, schema.clone()));
                self.sql_asked.remove(&(conn, schema.clone()));
                self.sql_schema.remove(&(conn, schema));
            }
            Ddl::DroppedTable => {
                self.tabs.retain(|t| !gone(t));
                self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                self.side_act(conn, SideAct::Refresh(schema));
            }
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
    pub(super) fn sql_drop_run(&mut self, conn: u64, schema: String, table: String, kind: DropKind) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return self.err("Not connected.") };
        self.worker.sql_drop(conn, sql, schema, table, kind == DropKind::View, kind == DropKind::Truncate, false);
    }

    pub(super) fn sql_new_db_dialog(&mut self, ui: &mut egui::Ui, f: &mut NewDb, keep: &mut bool) {
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
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let (Some(meta), Some(sql)) = (f.meta.clone(), self.conn(f.conn).and_then(|c| c.sql.clone())) else { return };
        self.worker.sql_create_db(f.conn, sql, f.name.clone(), f.charset.clone(), f.collation.clone(), meta, !self.conn_writable(f.conn));
    }

    pub(super) fn sql_new_table_dialog(&mut self, ui: &mut egui::Ui, f: &mut NewTable, keep: &mut bool) {
        ui.set_max_width(740.0);
        ui.set_width(700.0);
        let writable = self.conn_writable(f.conn);
        dialog_title(ui, "New table");
        ui.label(RichText::new(f.schema.as_str()).color(MUTED()));
        ui.add_space(8.0);
        let d = self.conn(f.conn).and_then(|c| c.sql.as_ref().map(|s| s.dialect)).unwrap_or_default();
        let bs = self.conn(f.conn).and_then(|c| c.sql.as_ref().map(|s| s.backslash_escapes())).unwrap_or(true);
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
            if ui.button("+ Column").clicked() {
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
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let (Some(meta), Some(sql)) = (f.meta.clone(), self.conn(f.conn).and_then(|c| c.sql.clone())) else { return };
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

    pub(super) fn sql_drop_dialog(&mut self, ui: &mut egui::Ui, f: &mut DropForm, keep: &mut bool) {
        ui.set_width(560.0);
        let writable = self.conn_writable(f.conn);
        let conn = self.conn_name(f.conn);
        let tag = self.conn(f.conn).map(|c| c.meta.tag.clone()).unwrap_or_default();
        let (name, noun, button) = match f.kind {
            DropKind::Database => (f.schema.as_str(), "database", "Drop database"),
            DropKind::Table => (f.table.as_str(), "table", "Drop table"),
            DropKind::View => (f.table.as_str(), "view", "Drop view"),
            DropKind::Truncate => (f.table.as_str(), "table", "Empty table"),
        };
        dialog_title(ui, if f.kind == DropKind::Truncate { format!("Empty table {name}?") } else { format!("Drop {noun} {name}?") });
        let wrap = |ui: &mut egui::Ui, text: String, color: Color32| {
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        };
        let mut loaded = false;
        match &f.info {
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
            Some(Ok(info)) => {
                loaded = true;
                let about = if info.rows_estimated { "about " } else { "" };
                let rows = |n: u64| format!("{about}{}", plural(n, "row"));
                let lead = match f.kind {
                    DropKind::Database => {
                        let mut parts = vec![plural(info.tables, "table")];
                        for (n, w) in [(info.views, "view"), (info.triggers, "trigger"), (info.routines, "routine"), (info.events, "event")] {
                            if n > 0 {
                                parts.push(plural(n, w));
                            }
                        }
                        let rows = info.rows.filter(|n| *n > 0).map(|n| format!(", {}", rows(n))).unwrap_or_default();
                        format!("This deletes the database {name} on {conn} with {}{rows}. Everything in it goes. It cannot be undone.", list_and(&parts))
                    }
                    DropKind::Table => match info.rows {
                        Some(0) => format!("This deletes the empty table and its structure on {conn}. It cannot be undone."),
                        Some(n) => format!("This deletes {} and the table's structure on {conn}. It cannot be undone.", rows(n)),
                        None => format!("This deletes the table and its structure on {conn}. It cannot be undone."),
                    },
                    DropKind::View => format!("This deletes the view on {conn}. The tables it reads from are not touched. It cannot be undone."),
                    DropKind::Truncate => match info.rows {
                        Some(n) => format!("This deletes all {} of {name} on {conn}. The table and its structure stay and the auto-increment counter restarts. It cannot be undone.", rows(n)),
                        None => format!("This deletes every row of {name} on {conn}. The table stays. It cannot be undone."),
                    },
                };
                wrap(ui, lead, TEXT());
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
        ui.add(egui::TextEdit::singleline(&mut f.typed).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
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
        let ok = writable && loaded && f.typed == name;
        right_actions(ui, |ui| {
            if ui.add_enabled_ui(ok, |ui| danger(ui, button)).inner.clicked() {
                *keep = false;
                self.danger(Danger::SqlDrop { conn: f.conn, schema: f.schema.clone(), table: f.table.clone(), kind: f.kind });
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    /// QUARRY_TEST_SQL=newdb, newtable or droptable: the dialogs of docs/sql/designs.figcraft, "Create and drop".
    pub(super) fn sql_create_demo(&mut self, which: &str) {
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
                schema: "shop".into(),
                table: "orders".into(),
                kind: DropKind::Table,
                typed: "order".into(),
                info: Some(Ok(DropInfo { exists: true, rows: Some(4410), referenced_by: vec!["order_items".into(), "payments".into(), "shipments".into()], ..Default::default() })),
            })),
        });
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
