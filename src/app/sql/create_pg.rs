//! Creating and dropping on PostgreSQL: the New database, New schema and New table dialogs, the extra options of the drop
//! confirmation (CASCADE, WITH (FORCE)) and what the app does when an answer arrives. The statements come from
//! `db::sql::pg::create`. MySQL's dialogs are in `sql_create.rs`; this server has no engines, charsets or collations to pick
//! and a table is built from the structure editor's column model.

use crate::app::sql::create::{list_and, plural, right_actions, sql_block, DropForm, DropKind, NOTE_DB, NOTE_TABLE};
use crate::app::*;
use db::sql::create::{Ddl, DropInfo};
use db::sql::pg::create::{self, Check, DbChoices, DbOpts, NewIndex, TableSpec, Unique};
use db::sql::ddl::{self, ColDef, Dv};
use db::sql::structure::{ForeignKey, Structure};

pub(in crate::app) struct PgNewDb {
    conn: u64,
    name: String,
    /// The template is None for the server's own default, template1, and every other None is the template's own setting.
    pub(in crate::app) opts: DbOpts,
    /// None while the server's lists load.
    pub(in crate::app) choices: Option<DbChoices>,
    load_err: Option<String>,
    focused: bool,
}

pub(in crate::app) struct PgNewSchema {
    conn: u64,
    database: String,
    name: String,
    focused: bool,
}

/// One column of the builder: the structure editor's definition and the two marks the table level needs.
#[derive(Clone, PartialEq)]
pub(in crate::app) struct PgCol {
    def: ColDef,
    pk: bool,
    unique: bool,
}

/// A foreign key of the builder, with the parent's structure as the server gave it, read on a request of its own like the Add
/// relation dialog's. `key.cols` and `key.ref_cols` are kept the same length, one pair per line, empty until picked.
#[derive(Clone)]
pub(in crate::app) struct PgFk {
    key: ForeignKey,
    /// The key points at the table being made, which follows the name typed.
    own: bool,
    pub(in crate::app) parent: Option<Structure>,
    /// The schema and table `parent` was asked for, and the request that carries the answer.
    asked: (String, String),
    req: u64,
    err: Option<String>,
}

impl PgFk {
    fn new(schema: &str) -> Self {
        let key = ForeignKey { ref_db: schema.to_string(), cols: vec![String::new()], ref_cols: vec![String::new()], on_update: "NO ACTION".into(), on_delete: "NO ACTION".into(), ..Default::default() };
        PgFk { key, own: false, parent: None, asked: Default::default(), req: 0, err: None }
    }
}

pub(in crate::app) struct PgNewTable {
    conn: u64,
    database: String,
    schema: String,
    name: String,
    cols: Vec<PgCol>,
    /// The UNIQUE constraints over several columns; one over a single column is also its column's Unique box.
    uniques: Vec<Unique>,
    checks: Vec<Check>,
    pub(in crate::app) fks: Vec<PgFk>,
    indexes: Vec<NewIndex>,
    comment: String,
    show_sql: bool,
    focused: bool,
    /// The statements of the form as it was last built, with the spec they came from.
    pub(in crate::app) built: Option<(TableSpec, db::Res<Vec<String>>)>,
}

/// The types the picker offers. The type box takes any other name, and the server checks it before anything is made.
const TYPES: [&str; 17] = ["integer", "bigint", "smallint", "text", "varchar(255)", "numeric(10,2)", "boolean", "date", "timestamp", "timestamptz", "time", "uuid", "jsonb", "json", "real", "double precision", "bytea"];

/// The actions of a key, PostgreSQL's default first.
const FK_ACTIONS: [&str; 5] = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

impl PgNewTable {
    fn new(conn: u64, database: String, schema: String) -> Self {
        PgNewTable { conn, database, schema, name: String::new(), cols: vec![id_col()], uniques: Vec::new(), checks: Vec::new(), fks: Vec::new(), indexes: Vec::new(), comment: String::new(), show_sql: false, focused: false, built: None }
    }

    /// Builds the statements again only when the spec changed since the last frame.
    fn refresh_built(&mut self) {
        let spec = self.spec();
        if self.built.as_ref().is_some_and(|(k, _)| *k == spec) {
            return;
        }
        let stmts = create::create_table_stmts(&spec);
        self.built = Some((spec, stmts));
    }

    fn spec(&self) -> TableSpec {
        let cols: Vec<ColDef> = self.cols.iter().map(|c| ColDef { nullable: c.def.nullable && !c.pk && !c.def.auto_increment, ..c.def.clone() }).collect();
        let marked = self.cols.iter().filter(|c| c.unique).map(|c| Unique { name: String::new(), cols: vec![c.def.name.clone()] });
        let fks = self
            .fks
            .iter()
            .map(|k| {
                let mut key = k.key.clone();
                if k.own {
                    (key.ref_db, key.ref_table) = (self.schema.clone(), self.name.clone());
                }
                // The parent as the dialog saw it, so the transaction can refuse when it changed before the table was made.
                key.ref_seen = k.parent.as_ref().filter(|p| !k.own && p.pg_schema.is_some()).map(|p| ddl::ref_state(p, &key.ref_cols)).unwrap_or_default();
                key
            })
            .collect();
        TableSpec { schema: self.schema.clone(), name: self.name.clone(), cols, pk: self.cols.iter().filter(|c| c.pk).map(|c| c.def.name.clone()).collect(), uniques: marked.chain(self.uniques.iter().cloned()).collect(), checks: self.checks.clone(), fks, indexes: self.indexes.clone(), comment: self.comment.clone() }
    }

    /// The first thing stopping a key that the statements cannot see: a part not picked, a parent not read yet or one that does not
    /// fit. The second value is true while a parent is still loading, which is not a mistake to point at.
    fn key_problem(&self, spec: &TableSpec) -> Option<(String, bool)> {
        let own = spec.as_structure();
        for (i, (k, key)) in self.fks.iter().zip(&spec.fks).enumerate() {
            let n = i + 1;
            if key.ref_table.is_empty() {
                return Some((format!("Pick the table foreign key {n} points at."), false));
            }
            if key.cols.iter().chain(&key.ref_cols).any(|c| c.is_empty()) {
                return Some((format!("Pick both columns of every line of foreign key {n}."), false));
            }
            if k.own {
                continue;
            }
            if let Some(e) = &k.err {
                return Some((format!("Could not read {}: {e}", key.ref_table), false));
            }
            match &k.parent {
                None => return Some((format!("Reading {}...", key.ref_table), true)),
                Some(p) => {
                    if let Some(why) = ddl::fk_problem(&own, &spec.name, p, key, false) {
                        return Some((format!("Foreign key {n}: {why}"), false));
                    }
                }
            }
        }
        None
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

    /// A column was renamed from `from`. Its picks follow, unless another column still has that name and the picks may be its. An
    /// emptied name is a removal, because an empty pick on a key line means nothing picked.
    fn col_renamed(&mut self, from: &str, to: &str) {
        if from.is_empty() || self.cols.iter().any(|c| c.def.name == from) {
            return;
        }
        if to.is_empty() {
            return self.forget_col(from);
        }
        self.picks_mut().filter(|p| *p == from).for_each(|p| *p = to.to_string());
    }

    /// Takes a removed column out of every list. A UNIQUE or index pick list hides a name it does not offer, so a stale one could
    /// neither be seen nor removed and a new column of that name would silently take its place. A key line is kept and left unpicked.
    fn forget_col(&mut self, name: &str) {
        for v in self.uniques.iter_mut().map(|u| &mut u.cols).chain(self.indexes.iter_mut().map(|i| &mut i.cols)) {
            v.retain(|c| c != name);
        }
        self.picks_mut().filter(|p| *p == name).for_each(String::clear);
    }

    fn remove_col(&mut self, i: usize) {
        let gone = self.cols.remove(i).def.name;
        if !self.cols.iter().any(|c| c.def.name == gone) {
            self.forget_col(&gone);
        }
    }

    /// The answer to a parent's structure read, when it is for one of this dialog's keys.
    pub(in crate::app) fn parent_loaded(&mut self, req: u64, res: &db::Res<Structure>) -> bool {
        let Some(k) = self.fks.iter_mut().find(|k| k.req == req && req != 0) else { return false };
        match res {
            Ok(s) => (k.parent, k.err) = (Some(s.clone()), None),
            Err(e) => (k.parent, k.err) = (None, Some(e.clone())),
        }
        true
    }
}

/// The first column of a new table: an integer identity primary key, which is what most tables want.
fn id_col() -> PgCol {
    PgCol { def: ColDef { auto_increment: true, nullable: false, ..ColDef::new("id", "integer") }, pk: true, unique: false }
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

/// A combo over plain strings, with `none` as the first item standing for no pick.
fn str_pick(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, label: &str, width: f32, value: &mut Option<String>, none: &str, items: &[String]) {
    let shown = value.clone().unwrap_or_else(|| none.to_string());
    let r = egui::ComboBox::from_id_salt(id).width(width).selected_text(shown).show_ui(ui, |ui| {
        menu_look(ui);
        if ui.selectable_label(value.is_none(), none).clicked() {
            *value = None;
        }
        for k in items {
            if ui.selectable_label(value.as_deref() == Some(k.as_str()), k).clicked() {
                *value = Some(k.clone());
            }
        }
    });
    r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, label));
}

fn name_field(ui: &mut egui::Ui, text: &mut String, label: String) {
    ui.add(egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).desired_width(150.0).hint_text("server names it")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, &label));
}

fn editor_label(ui: &mut egui::Ui, text: &mut String, width: f32, label: &str) -> egui::Response {
    let r = ui.add(egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).desired_width(width));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
    r
}

impl App {
    pub(in crate::app) fn pg_session(&self, conn: u64) -> Option<db::sql::Db> {
        if self.demo {
            return None;
        }
        self.conn(conn)?.sql.clone()
    }

    /// The schema the sidebar lists for `db`, which is where a new table or a drop applies.
    fn pg_schema_of(&self, conn: u64, db: &str) -> String {
        Some(self.ns_of(conn, db)).filter(|s| !s.is_empty()).unwrap_or(PG_PUBLIC).to_string()
    }

    pub(in crate::app) fn pg_new_db(&mut self, conn: u64) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_new_db(conn));
        }
        if let Some(sql) = self.pg_session(conn) {
            self.worker.pg_db_choices(conn, sql);
        }
        self.modal = Some(Modal::PgNewDb(Box::new(PgNewDb { conn, name: String::new(), opts: DbOpts::default(), choices: None, load_err: None, focused: false })));
    }

    pub(in crate::app) fn pg_choices_done(&mut self, conn: u64, res: db::Res<DbChoices>) {
        if let Some(Modal::PgNewDb(f)) = &mut self.modal && f.conn == conn {
            match res {
                Ok(t) => f.choices = Some(t),
                Err(e) => f.load_err = Some(e),
            }
        }
    }

    pub(in crate::app) fn pg_new_schema(&mut self, conn: u64, database: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_new_schema(conn, database));
        }
        self.modal = Some(Modal::PgNewSchema(Box::new(PgNewSchema { conn, database, name: String::new(), focused: false })));
    }

    pub(in crate::app) fn pg_new_table(&mut self, conn: u64, database: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_new_table(conn, database));
        }
        let schema = self.pg_schema_of(conn, &database);
        self.modal = Some(Modal::PgNewTable(Box::new(PgNewTable::new(conn, database, schema))));
    }

    /// Asks what a drop takes. `name` is the schema for a schema, the table or view for those, and empty for a database.
    pub(in crate::app) fn pg_drop_ask(&mut self, conn: u64, db: String, name: String, kind: DropKind) {
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
        if let Some(why) = self.pg_job_blocks(conn, &database, &schema, kind) {
            return self.warn(why);
        }
        if let Some(sql) = self.pg_session(conn) {
            self.worker.pg_drop_info(conn, sql, database.clone(), schema.clone(), table.clone(), !self.conn_writable(conn));
        }
        self.modal = Some(Modal::SqlDrop(Box::new(DropForm { conn, database, schema, table, kind, typed: String::new(), opt: false, info: None })));
    }

    /// Why a drop cannot start: an import, script file or shell of Quarry's own is running in that database (or schema). The server would
    /// refuse the drop for its connections and blame another session, and WITH (FORCE) would kill the work halfway.
    /// A script can touch anything in its database, and an import is known by its schema only, so a table drop is held for both.
    fn pg_job_blocks(&self, conn: u64, database: &str, schema: &str, kind: DropKind) -> Option<String> {
        let job = self.sql_job.as_ref().filter(|j| j.conn == conn && j.db.pg_database() == database).filter(|j| kind == DropKind::Database || j.kind == sql::dump::JobKind::Script || j.schema == schema);
        let what = match job {
            Some(j) if j.kind == sql::dump::JobKind::Import => "an import",
            Some(_) => "a script file",
            None if self.tabs.iter().any(|t| t.sel.conn == conn && t.sel.db == database && t.shell.cancel.is_some()) => "a shell script",
            None => return None,
        };
        Some(format!("Quarry is running {what} in {database}. Stop it first, then drop."))
    }

    /// Runs a confirmed drop. Tabs and sidebar state go when the server says it worked (`pg_ddl_done`), never before: a drop that is
    /// refused (another session, a pooler) must leave the person's query text and staged changes where they were.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn pg_drop_run(&mut self, conn: u64, sql: db::sql::Db, database: String, schema: String, table: String, kind: DropKind, opt: bool) {
        // A job can start while the confirmation is open, so the check is made again here.
        if let Some(why) = self.pg_job_blocks(conn, &database, &schema, kind) {
            return self.warn(why);
        }
        // The worker checks again so a lock taken while the confirmation was open still holds.
        let ro = !self.conn_writable(conn);
        match kind {
            DropKind::Database => self.worker.pg_drop_db(conn, sql, database, opt, ro),
            DropKind::Schema => self.worker.pg_drop_schema(conn, sql.at(&database), schema, opt, ro),
            DropKind::Table | DropKind::View => self.worker.pg_drop_table(conn, sql.at(&database), schema, table, kind == DropKind::View, opt, ro),
            DropKind::Truncate => unreachable!("the PostgreSQL menus never offer Empty table"),
        }
    }

    /// What the open tabs of a database hold that closing them would lose: staged row edits, staged structure changes and Query text.
    pub(in crate::app) fn pg_unsaved_in(&self, conn: u64, database: &str) -> (usize, usize, usize) {
        let edits = self.drafts.values().filter(|d| d.conn == conn && d.db == database).count();
        let mut staged = 0;
        let mut queries = 0;
        for t in self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == database) {
            if let Some(s) = &t.sql {
                staged += s.st.staged.len();
                queries += usize::from(!s.query.trim().is_empty());
            }
        }
        (edits, staged, queries)
    }

    /// A create or drop finished: the sidebar lists and the tabs follow what the server has now.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn pg_ddl_done(&mut self, conn: u64, database: String, schema: String, table: String, what: Ddl, res: db::Res<String>, dbs: Option<Vec<String>>) {
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
                self.purge_undo(conn, &schema, "", "");
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
                self.purge_undo(conn, &database, &schema, "");
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
                self.purge_undo(conn, &database, &schema, &table);
                self.remove_tabs(|t| in_schema(t, &schema) && t.sel.coll == table);
                self.pg_refresh(conn, &database);
            }
            Ddl::Emptied => unreachable!("PostgreSQL never empties a table through the drop path"),
        }
    }

    pub(in crate::app) fn pg_new_db_dialog(&mut self, ui: &mut egui::Ui, f: &mut PgNewDb, keep: &mut bool) {
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
            if let Some(ch) = &f.choices {
                form_label(ui, "Owner");
                let me = format!("{} (this connection)", ch.me);
                let owners: Vec<String> = ch.owners.iter().filter(|r| **r != ch.me).cloned().collect();
                str_pick(ui, "pg-newdb-owner", "Owner", 400.0, &mut f.opts.owner, &me, &owners);
                ui.end_row();
                form_label(ui, "Template");
                let before = f.opts.template.clone();
                let names: Vec<String> = ch.templates.iter().map(|t| t.name.clone()).collect();
                str_pick(ui, "pg-newdb-template", "Template", 400.0, &mut f.opts.template, "Server default (template1)", &names);
                // The encoding and locale picks belong to template0, so another template drops them rather than keep a choice it refuses.
                if f.opts.template != before && f.opts.template.as_deref() != Some(create::PRISTINE) {
                    f.opts = DbOpts { template: f.opts.template.take(), owner: f.opts.owner.take(), ..Default::default() };
                }
                ui.end_row();
            }
        });
        if let Some(ch) = f.choices.clone() {
            pg_locale_section(ui, &mut f.opts, &ch);
        }
        ui.add_space(8.0);
        let choices = f.choices.clone().unwrap_or_default();
        let built = create::create_db_sql(&f.name, &f.opts, &choices);
        match (&built, &f.load_err) {
            (_, Some(e)) => {
                ui.label(RichText::new(format!("Could not read the server's templates, encodings and roles: {e}")).size(TXT_SMALL).color(ALERT()));
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
        ui.label(RichText::new("A database cannot be locked, so undo removes it only while it is empty and nobody else is connected.").size(TXT_SMALL).color(FAINT()));
        if !writable {
            ui.label(RichText::new(NOTE_DB).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && f.choices.is_some() && built.is_ok();
        right_actions(ui, |ui| {
            if ui.add_enabled(ok, primary_button(ui, "Create", PRIMARY())).clicked() {
                if let Some(sql) = self.sql_db_for(f.conn, None) {
                    self.worker.pg_create_db(f.conn, sql, f.name.clone(), f.opts.clone(), !writable);
                }
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    pub(in crate::app) fn pg_new_schema_dialog(&mut self, ui: &mut egui::Ui, f: &mut PgNewSchema, keep: &mut bool) {
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
        let built = create::create_schema_sql(&f.name);
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

    pub(in crate::app) fn pg_new_table_dialog(&mut self, ui: &mut egui::Ui, f: &mut PgNewTable, keep: &mut bool) {
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
        ui.horizontal(|ui| {
            if add_chip(ui, "Column").clicked() {
                let n = f.cols.len() + 1;
                f.cols.push(PgCol { def: ColDef::new(&format!("column_{n}"), "text"), pk: false, unique: false });
            }
            ui.label(RichText::new("Columns, keys and indexes can be changed later in Structure").size(TXT_SMALL).color(FAINT()));
        });
        ui.add_space(10.0);
        caption(ui, "KEYS, CHECKS AND INDEXES");
        ui.add_space(4.0);
        self.pg_constraint_list(ui, f);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if add_chip(ui, "Unique").clicked() {
                f.uniques.push(Unique::default());
            }
            if add_chip(ui, "Check").clicked() {
                f.checks.push(Check::default());
            }
            if add_chip(ui, "Foreign key").clicked() {
                f.fks.push(PgFk::new(&f.schema));
            }
            if add_chip(ui, "Index").clicked() {
                f.indexes.push(NewIndex::default());
            }
        });
        f.refresh_built();
        let (spec, built) = f.built.as_ref().unwrap();
        let keys = f.key_problem(spec);
        if f.show_sql {
            ui.add_space(6.0);
            match built {
                Ok(stmts) => sql_block(ui, &stmts.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")),
                Err(_) => {
                    ui.label(RichText::new("The statement appears here once the form is valid.").size(TXT_SMALL).color(FAINT()));
                }
            }
        }
        match (built, &keys) {
            (Err(e), _) if !f.name.is_empty() => {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
            }
            (_, Some((e, loading))) => {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(if *loading { FAINT() } else { ALERT() }));
            }
            _ => {}
        }
        if !spec.fks.is_empty() {
            ui.label(RichText::new("Each foreign key locks the table it points at while the table is made. PostgreSQL makes no index for the referencing columns; add one under Index if rows are looked up by them.").size(TXT_SMALL).color(FAINT()));
        }
        if !writable {
            ui.label(RichText::new(NOTE_TABLE).size(TXT_SMALL).color(FAINT()));
        }
        let ok = writable && built.is_ok() && keys.is_none();
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
        // The names before this frame's typing, so a renamed column takes its picks in keys and indexes with it.
        let names: Vec<String> = f.cols.iter().map(|c| c.def.name.clone()).collect();
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
        for (i, from) in names.iter().enumerate() {
            if f.cols[i].def.name != *from {
                let to = f.cols[i].def.name.clone();
                f.col_renamed(from, &to);
            }
        }
        if let Some(i) = remove {
            f.remove_col(i);
        }
    }

    /// The UNIQUE constraints over several columns, the CHECKs, the foreign keys and the indexes of the new table, one row each with its
    /// name (empty lets the server name it), what it holds, and a remove button. A foreign key's parent is read when it is picked.
    fn pg_constraint_list(&mut self, ui: &mut egui::Ui, f: &mut PgNewTable) {
        if f.uniques.is_empty() && f.checks.is_empty() && f.fks.is_empty() && f.indexes.is_empty() {
            ui.label(RichText::new("None yet. A column's Unique box is a single-column UNIQUE; add one over several columns, a CHECK, a foreign key or an index here.").size(TXT_SMALL).color(FAINT()));
            return;
        }
        let cols: Vec<String> = f.cols.iter().map(|c| c.def.name.clone()).filter(|n| !n.is_empty()).collect();
        let schemas: Vec<String> = self.conn(f.conn).and_then(|c| c.pg.get(&f.database)).map(|p| p.schemas.iter().filter(|s| !s.system || s.name == f.schema).map(|s| s.name.clone()).collect()).unwrap_or_else(|| vec![f.schema.clone()]);
        // The tables of each schema a key points at, loading the list the first time it is asked.
        let mut tables: HashMap<String, Option<Vec<String>>> = HashMap::new();
        for k in &f.fks {
            if !tables.contains_key(&k.key.ref_db) {
                let t = self.fk_tables(f.conn, &f.database, &k.key.ref_db);
                tables.insert(k.key.ref_db.clone(), t);
            }
        }
        let (mut drop_u, mut drop_c, mut drop_f, mut drop_i) = (None, None, None, None);
        let this = format!("{} (this table)", if f.name.is_empty() { "new table" } else { f.name.as_str() });
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().id_salt("pg-newtable-keys").max_height(230.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("pg-newtable-keys-grid").num_columns(3).spacing([10.0, 8.0]).show(ui, |ui| {
                    let remove = |ui: &mut egui::Ui, what: &str| ui.add(quiet_button(RichText::new("remove").size(TXT_CAPTION).color(FAINT()))).on_hover_text(format!("Remove this {what}")).clicked();
                    for (i, u) in f.uniques.iter_mut().enumerate() {
                        let n = i + 1;
                        caption(ui, "UNIQUE");
                        ui.horizontal(|ui| {
                            name_field(ui, &mut u.name, format!("Unique name {n}"));
                            cols_pick(ui, &format!("Unique columns {n}"), &cols, &mut u.cols);
                        });
                        if remove(ui, "constraint") {
                            drop_u = Some(i);
                        }
                        ui.end_row();
                    }
                    for (i, c) in f.checks.iter_mut().enumerate() {
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
                    for (i, k) in f.fks.iter_mut().enumerate() {
                        let n = i + 1;
                        caption(ui, "FOREIGN KEY");
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                name_field(ui, &mut k.key.name, format!("Foreign key name {n}"));
                                let before = (k.key.ref_db.clone(), k.key.ref_table.clone(), k.own);
                                let r = egui::ComboBox::from_id_salt(("pg-newtable-fk-schema", i)).width(110.0).selected_text(k.key.ref_db.as_str()).show_ui(ui, |ui| {
                                    menu_look(ui);
                                    for s in &schemas {
                                        ui.selectable_value(&mut k.key.ref_db, s.clone(), s.as_str());
                                    }
                                });
                                r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Referenced schema {n}")));
                                let list = tables.get(&k.key.ref_db).cloned().flatten();
                                let shown = if k.own { this.clone() } else if !k.key.ref_table.is_empty() { k.key.ref_table.clone() } else if list.is_none() { "Loading tables...".into() } else { "Pick a table".into() };
                                let r = egui::ComboBox::from_id_salt(("pg-newtable-fk-table", i)).width(160.0).selected_text(shown).show_ui(ui, |ui| {
                                    menu_look(ui);
                                    if k.key.ref_db == f.schema && ui.selectable_label(k.own, this.as_str()).clicked() {
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
                            let parent_cols: Vec<String> = if k.own { cols.clone() } else { k.parent.as_ref().map(|p| p.columns.iter().map(|c| c.name.clone()).collect()).unwrap_or_default() };
                            let lines = k.key.cols.len();
                            let mut cut = None;
                            for j in 0..lines {
                                ui.horizontal(|ui| {
                                    for (side, list, label) in [(0, &cols, "Column"), (1, &parent_cols, "Referenced column")] {
                                        let v = if side == 0 { &mut k.key.cols[j] } else { &mut k.key.ref_cols[j] };
                                        let r = egui::ComboBox::from_id_salt(("pg-newtable-fk-col", i, j, side)).width(150.0).selected_text(if v.is_empty() { "Column".to_string() } else { v.clone() }).show_ui(ui, |ui| {
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
                                    let r = egui::ComboBox::from_id_salt(("pg-newtable-fk-act", i, label)).width(100.0).selected_text(v.as_str()).show_ui(ui, |ui| {
                                        menu_look(ui);
                                        for a in FK_ACTIONS {
                                            ui.selectable_value(v, a.to_string(), a);
                                        }
                                    });
                                    r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Foreign key {n} {label}")));
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut k.key.match_full, "Match full").on_hover_text("A key with several columns must be all NULL or all set. The default lets any NULL column skip the check.");
                                ui.checkbox(&mut k.key.deferrable, "Deferrable").on_hover_text("Lets a transaction postpone the check to its end.");
                                ui.add_enabled(k.key.deferrable, egui::Checkbox::new(&mut k.key.deferred, "Initially deferred"));
                                if !k.key.deferrable {
                                    k.key.deferred = false;
                                }
                            });
                        });
                        if remove(ui, "foreign key") {
                            drop_f = Some(i);
                        }
                        ui.end_row();
                    }
                    for (i, x) in f.indexes.iter_mut().enumerate() {
                        let n = i + 1;
                        caption(ui, "INDEX");
                        ui.horizontal(|ui| {
                            name_field(ui, &mut x.name, format!("Index name {n}"));
                            cols_pick(ui, &format!("Index columns {n}"), &cols, &mut x.cols);
                            ui.checkbox(&mut x.unique, "Unique").widget_info(|| egui::WidgetInfo::labeled(egui::Role::CheckBox, true, format!("Unique index {n}")));
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
            f.uniques.remove(i);
        }
        if let Some(i) = drop_c {
            f.checks.remove(i);
        }
        if let Some(i) = drop_f {
            f.fks.remove(i);
        }
        if let Some(i) = drop_i {
            f.indexes.remove(i);
        }
        // Each key's parent, read once per pick on a request of its own.
        for k in f.fks.iter_mut() {
            let want = (k.key.ref_db.clone(), k.key.ref_table.clone());
            if k.own || want.1.is_empty() || want == k.asked {
                continue;
            }
            (k.asked, k.parent, k.err) = (want.clone(), None, None);
            if let Some(sql) = self.pg_session(f.conn) {
                self.req += 1;
                k.req = self.req;
                self.worker.sql_structure(self.req, sql.at(&f.database), want.0, want.1);
            }
        }
    }

    /// The body of a PostgreSQL drop confirmation: what goes, what a drop cannot do, and the opt-in that widens it (CASCADE or
    /// WITH (FORCE)), which is ticked in this dialog and never remembered. True when the drop cannot run as things stand.
    pub(in crate::app) fn pg_drop_notes(&mut self, ui: &mut egui::Ui, f: &mut DropForm, info: &DropInfo, name: &str, conn: &str) -> bool {
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
                wrap(ui, "Its open tabs here are closed once it is dropped, and Quarry's own connections to it are closed first.".into(), MUTED());
                let (edits, staged, queries) = self.pg_unsaved_in(f.conn, &f.database);
                if edits + staged + queries > 0 {
                    let mut parts = Vec::new();
                    if edits > 0 {
                        parts.push(plural(edits as u64, "staged row edit"));
                    }
                    if staged > 0 {
                        parts.push(plural(staged as u64, "staged structure change"));
                    }
                    if queries > 0 {
                        parts.push(plural(queries as u64, "query with text in its editor"));
                    }
                    wrap(ui, format!("Its tabs hold {}, which are lost with them when the drop works.", list_and(&parts)), WARNING());
                }
                if let Some(n) = info.sessions.filter(|n| *n > 0) {
                    wrap(ui, format!("{} connected to it right now (this app's own among them, if any). PostgreSQL refuses the drop while anyone is connected.", plural(n, "session")), MUTED());
                }
                ui.add_space(4.0);
                ui.checkbox(&mut f.opt, "Disconnect the other sessions (WITH (FORCE))").on_hover_text("They lose their work in progress. Needs the right to end their sessions.");
            }
            DropKind::Schema | DropKind::Table | DropKind::View => {
                let what = match f.kind {
                    DropKind::Schema => format!("the schema {name} in {}", f.database),
                    // The same name can be in several schemas and databases, so the sentence says which one.
                    DropKind::View => format!("the view {}.{}.{name}", f.database, f.schema),
                    _ => format!("the table {}.{}.{name}", f.database, f.schema),
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
                    // Read off the server by running the drop CASCADE and rolling it back, so the list is what the server takes and not a guess. The listing takes the locks the drop would take, for a moment.
                    let lead = format!("Dropping it also drops {}", plural(info.dependents_total, "object"));
                    wrap(ui, format!("{lead}: {}{tail}. Listing them took the locks the drop would take, for a moment, and gave them back.", list_and(&info.dependents)), MUTED());
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
            DropKind::Truncate => unreachable!("the PostgreSQL menus never offer Empty table"),
        }
        blocked
    }
}

/// The Encoding and locale section of the New database dialog. A copy of any template but template0 keeps that template's encoding and
/// locale, and the server refuses another, so the picks are offered on template0 only; on any other the section says what the copy
/// gets and how to change it.
fn pg_locale_section(ui: &mut egui::Ui, o: &mut DbOpts, ch: &DbChoices) {
    ui.add_space(10.0);
    caption(ui, "ENCODING AND LOCALE");
    ui.add_space(4.0);
    let note = |ui: &mut egui::Ui, text: String| {
        ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(FAINT())).wrap());
    };
    let tpl = ch.template(o.template.as_deref()).cloned().unwrap_or_default();
    let locale_of = |t: &create::Template| if t.collate == t.ctype { t.collate.clone() } else { format!("{} (LC_COLLATE), {} (LC_CTYPE)", t.collate, t.ctype) };
    if o.template.as_deref() != Some(create::PRISTINE) {
        let icu = match tpl.provider.as_str() {
            "i" => format!(", ICU locale {}", tpl.icu),
            "b" => format!(", builtin locale {}", tpl.icu),
            _ => String::new(),
        };
        if !tpl.name.is_empty() {
            ui.label(RichText::new(format!("A copy of {} gets {}, locale {}{icu}.", tpl.name, tpl.encoding, locale_of(&tpl))).color(MUTED()));
        }
        note(ui, "Another encoding or locale needs TEMPLATE template0: the server refuses one that differs from the template's, because a copy of any other template keeps its own.".into());
        if ch.templates.iter().any(|t| t.name == create::PRISTINE) && quiet(ui, "Use template0").clicked() {
            o.template = Some(create::PRISTINE.into());
        }
        return;
    }
    let enc_before = o.encoding.clone();
    egui::Grid::new("pg-newdb-locale").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, |ui| {
        form_label(ui, "Encoding");
        str_pick(ui, "pg-newdb-enc", "Encoding", 400.0, &mut o.encoding, &format!("Template's ({})", tpl.encoding), &ch.encodings);
        ui.end_row();
        let enc = o.encoding.clone().unwrap_or_else(|| tpl.encoding.clone());
        let locales = ch.locales_with(&enc, Some(&tpl));
        // A locale made for another encoding would be refused by the server, so a change of encoding drops it.
        if o.encoding != enc_before {
            for l in [&mut o.locale, &mut o.lc_collate, &mut o.lc_ctype] {
                if l.as_ref().is_some_and(|v| !locales.contains(v)) {
                    *l = None;
                }
            }
        }
        // Where LOCALE would also set the ICU or builtin locale, only LC_COLLATE and LC_CTYPE are offered.
        let word_ok = ch.locale_word_ok(o);
        if !word_ok {
            o.locale = None;
        }
        form_label(ui, "Locale");
        ui.add_enabled_ui(word_ok, |ui| str_pick(ui, "pg-newdb-locale", "Locale", 400.0, &mut o.locale, &format!("Template's ({})", locale_of(&tpl)), &locales));
        ui.end_row();
        // LOCALE sets both of these, and the server refuses it together with either.
        if o.locale.is_some() {
            (o.lc_collate, o.lc_ctype) = (None, None);
        }
        for (label, v, own) in [("LC_COLLATE", &mut o.lc_collate, &tpl.collate), ("LC_CTYPE", &mut o.lc_ctype, &tpl.ctype)] {
            form_label(ui, label);
            ui.add_enabled_ui(o.locale.is_none(), |ui| str_pick(ui, ("pg-newdb", label), label, 400.0, v, &format!("Template's ({own})"), &locales));
            ui.end_row();
        }
        if ch.icu_ok() {
            form_label(ui, "Provider");
            let own = create::provider_name(&tpl.provider);
            str_pick(ui, "pg-newdb-provider", "Locale provider", 400.0, &mut o.provider, &format!("Template's ({own})"), &["libc".to_string(), "icu".to_string()]);
            ui.end_row();
            if o.provider.as_deref() == Some("icu") {
                form_label(ui, "ICU locale");
                str_pick(ui, "pg-newdb-icu", "ICU locale", 400.0, &mut o.icu_locale, "Pick one", &ch.icu_locales);
                ui.end_row();
            } else {
                o.icu_locale = None;
            }
        }
    });
    if !ch.locale_word_ok(o) {
        let p = create::provider_name(&ch.provider_of(o));
        note(ui, format!("Locale is off: on PostgreSQL 16 and later LOCALE also sets the {p} locale of a database with the {p} provider. LC_COLLATE and LC_CTYPE set the operating system locale alone."));
    }
    if let Some(why) = ch.template_locale_problem(o) {
        ui.add(egui::Label::new(RichText::new(why).size(TXT_SMALL).color(ALERT())).wrap());
    }
    if !ch.icu_ok() {
        note(ui, if ch.version < 150000 { "A locale provider and an ICU locale need PostgreSQL 15 or later." } else { "This server was built without ICU, so a database uses the operating system's locales." }.into());
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
        let t = PgNewTable { name: "t".into(), ..PgNewTable::new(1, "d".into(), "public".into()) };
        let s = create::create_table_stmts(&t.spec()).unwrap();
        assert_eq!(s[0], "CREATE TABLE \"public\".\"t\" (\n  \"id\" integer GENERATED BY DEFAULT AS IDENTITY,\n  PRIMARY KEY (\"id\")\n)");
    }

    #[test]
    fn a_renamed_or_removed_column_leaves_no_stale_pick_in_keys_and_indexes() {
        let mut t = PgNewTable { name: "t".into(), ..PgNewTable::new(1, "d".into(), "public".into()) };
        let col = |n: &str| PgCol { def: ColDef::new(n, "integer"), pk: false, unique: false };
        t.cols.extend([col("price"), col("qty")]);
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        t.uniques = vec![Unique { name: String::new(), cols: v(&["price", "qty"]) }];
        t.indexes = vec![NewIndex { name: String::new(), cols: v(&["qty", "price"]), unique: false }];
        let mut other = PgFk::new("public");
        (other.key.ref_table, other.key.cols, other.key.ref_cols) = ("parent".into(), v(&["price"]), v(&["price"]));
        let mut own = PgFk::new("public");
        own.own = true;
        (own.key.cols, own.key.ref_cols) = (v(&["qty"]), v(&["price"]));
        t.fks = vec![other, own];

        t.cols[1].def.name = "cost".into();
        t.col_renamed("price", "cost");
        assert_eq!(t.uniques[0].cols, v(&["cost", "qty"]));
        assert_eq!(t.indexes[0].cols, v(&["qty", "cost"]));
        assert_eq!((t.fks[0].key.cols.clone(), t.fks[0].key.ref_cols.clone()), (v(&["cost"]), v(&["price"])), "another table's column keeps its name");
        assert_eq!(t.fks[1].key.ref_cols, v(&["cost"]), "a key on this table follows the rename on both sides");

        t.remove_col(1);
        assert_eq!(t.uniques[0].cols, v(&["qty"]));
        assert_eq!(t.indexes[0].cols, v(&["qty"]));
        assert_eq!((t.fks[0].key.cols.clone(), t.fks[1].key.ref_cols.clone()), (v(&[""]), v(&[""])), "the key lines stay, unpicked");
        // A new column of the old name does not slip into the lists.
        t.cols.push(col("cost"));
        let spec = t.spec();
        assert_eq!((spec.uniques[0].cols.clone(), spec.indexes[0].cols.clone()), (v(&["qty"]), v(&["qty"])));

        // Two columns of one name: renaming one leaves the picks to the other.
        t.cols.push(col("cost"));
        t.cols[3].def.name = "x".into();
        t.indexes[0].cols = v(&["cost"]);
        t.col_renamed("cost", "x");
        assert_eq!(t.indexes[0].cols, v(&["cost"]));
    }

    #[test]
    fn sizes_read_in_the_largest_unit() {
        assert_eq!(human_bytes(900), "900 B");
        assert_eq!(human_bytes(8 * 1024 * 1024 + 300_000), "8.3 MB");
    }
}
