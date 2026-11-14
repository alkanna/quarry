//! Renaming and duplicating SQL tables and views, renaming a PostgreSQL schema, and emptying a PostgreSQL table: the three dialogs and
//! what the app does when the server answers. The statements come from `db::sql::tableops` (MySQL, MariaDB) and
//! `db::sql::pg::tableops`. MySQL's own Empty table is the drop dialog's (`DropKind::Truncate`).

use crate::app::sql::create::{list_and, plural, sql_block, DropKind};
use crate::app::*;
use db::sql::create::DropInfo;
use db::sql::pg::tableops as pgops;
use db::sql::tableops as myops;

const NOTE_WRITE: &str = "Read-only: unlock writes to change tables";

/// Rename a table, view or (PostgreSQL) schema. `database` is the PostgreSQL database and empty on MySQL, where `schema` is the database.
pub(in crate::app) struct RenameForm {
    conn: u64,
    database: String,
    schema: String,
    /// Table, View or Schema.
    kind: DropKind,
    from: String,
    to: String,
    focused: bool,
}

pub(in crate::app) struct DupForm {
    conn: u64,
    database: String,
    schema: String,
    from: String,
    to: String,
    data: bool,
    focused: bool,
}

/// Empty a PostgreSQL table, with the two options TRUNCATE has.
pub(in crate::app) struct TruncForm {
    conn: u64,
    database: String,
    schema: String,
    table: String,
    typed: String,
    restart: bool,
    cascade: bool,
    /// None while the counts load.
    info: Option<Result<DropInfo, String>>,
}

/// The first line of the empty confirmation, with the rows only where they are known: an estimate of 0 is not known, because the
/// server's statistics lag behind a table that filled since.
fn trunc_lead(table: &str, conn: &str, info: &DropInfo) -> String {
    let rows = match (info.rows, info.rows_estimated) {
        (Some(n), false) => format!(" ({})", plural(n, "row")),
        (Some(n), true) if n > 0 => format!(" (about {})", plural(n, "row")),
        _ => String::new(),
    };
    format!("This deletes every row of {table}{rows} on {conn}. The table and its structure stay. It cannot be undone.")
}

impl App {
    /// The tabs open on `table` of `schema` (every table of it when `table` is empty). On PostgreSQL a tab without a schema of its own
    /// belongs to the one the sidebar shows.
    fn tabs_on(&self, conn: u64, database: &str, schema: &str, table: &str) -> Vec<usize> {
        let on = |t: &Tab| t.sel.conn == conn && (if database.is_empty() { t.sel.db == schema } else { t.sel.db == database && self.sel_schema(&t.sel) == schema }) && (table.is_empty() || t.sel.coll == table);
        (0..self.tabs.len()).filter(|&i| on(&self.tabs[i])).collect()
    }

    /// Staged edits in a tab on the table are keyed by its name, so a rename under them would lose or misplace them.
    fn staged_on(&self, tabs: &[usize], conn: u64, table: &str) -> bool {
        let staged = tabs.iter().any(|&i| self.tabs[i].sql.as_ref().is_some_and(|s| !s.st.staged.is_empty()));
        staged || self.drafts.values().any(|d| d.conn == conn && (table.is_empty() || d.coll == table) && tabs.iter().any(|&i| self.tabs[i].sel.db == d.db && self.tabs[i].sel.schema == d.schema))
    }

    fn session_of(&self, conn: u64, database: &str) -> Option<db::sql::Db> {
        let sql = self.conn(conn).and_then(|c| c.sql.clone())?;
        Some(if database.is_empty() { sql } else { sql.at(database) })
    }

    /// `db` is the database the sidebar menu was opened on and `name` the table, view or schema.
    pub(in crate::app) fn sql_rename_ask(&mut self, conn: u64, db: String, name: String, kind: DropKind) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.sql_rename_ask(conn, db, name, kind));
        }
        let (database, schema) = match (self.is_pg(conn), kind) {
            (true, DropKind::Schema) => (db, name.clone()),
            (true, _) => {
                let s = self.sel_schema(&Sel { conn, db: db.clone(), ..Default::default() });
                (db, s)
            }
            (false, _) => (String::new(), db),
        };
        self.modal = Some(Modal::SqlRename(Box::new(RenameForm { conn, database, schema, kind, to: name.clone(), from: name, focused: false })));
    }

    pub(in crate::app) fn sql_duplicate_ask(&mut self, conn: u64, db: String, name: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.sql_duplicate_ask(conn, db, name));
        }
        let (database, schema) = if self.is_pg(conn) { (db.clone(), self.sel_schema(&Sel { conn, db, ..Default::default() })) } else { (String::new(), db) };
        self.modal = Some(Modal::SqlDuplicate(Box::new(DupForm { conn, database, schema, to: format!("{name}_copy"), from: name, data: true, focused: false })));
    }

    pub(in crate::app) fn pg_truncate_ask(&mut self, conn: u64, db: String, table: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.pg_truncate_ask(conn, db, table));
        }
        let schema = self.sel_schema(&Sel { conn, db: db.clone(), ..Default::default() });
        if let Some(sql) = self.session_of(conn, &db).filter(|_| !self.demo) {
            self.worker.pg_truncate_info(conn, sql, db.clone(), schema.clone(), table.clone());
        }
        self.modal = Some(Modal::PgTruncate(Box::new(TruncForm { conn, database: db, schema, table, typed: String::new(), restart: false, cascade: false, info: None })));
    }

    /// What emptying a table reaches arrived: the open dialog shows it.
    pub(in crate::app) fn truncate_info_done(&mut self, conn: u64, database: &str, schema: &str, table: &str, res: &db::Res<DropInfo>) {
        if let Some(Modal::PgTruncate(f)) = &mut self.modal && f.conn == conn && f.database == database && f.schema == schema && f.table == table {
            f.info = Some(res.clone());
        }
    }

    pub(in crate::app) fn sql_rename_dialog(&mut self, ui: &mut egui::Ui, f: &mut RenameForm, keep: &mut bool) {
        ui.set_width(520.0);
        let writable = self.conn_writable(f.conn);
        let (pg, d) = (self.is_pg(f.conn), self.dialect_of(f.conn));
        let noun = match f.kind {
            DropKind::Schema => "schema",
            DropKind::View => "view",
            _ => "table",
        };
        let place = match (pg, f.kind) {
            (false, _) => String::new(),
            (true, DropKind::Schema) => format!("in {} ", f.database),
            (true, _) => format!("in {}.{} ", f.database, f.schema),
        };
        dialog_head(ui, &Head::new(format!("Rename {noun} {}", f.from)).sub(format!("{place}on {}", self.conn_name(f.conn))).glyph(glyphs::pencil));
        let taken = f.kind != DropKind::Schema && f.to != f.from && self.tables_of(&Sel { conn: f.conn, db: if pg { f.database.clone() } else { f.schema.clone() }, schema: if pg { f.schema.clone() } else { String::new() }, coll: String::new() }).iter().any(|t| *t == f.to);
        let built = dialog_body(ui, |ui| {
            stacked_field(ui, &format!("New {noun} name"), "", |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut f.to).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "New name"));
                if !f.focused {
                    r.request_focus();
                    f.focused = true;
                }
            });
            ui.add_space(8.0);
            let built = match (pg, f.kind) {
                (true, DropKind::Schema) => pgops::rename_schema_sql(&f.from, &f.to),
                (true, k) => pgops::rename_sql(if k == DropKind::View { "VIEW" } else { "TABLE" }, &f.schema, &f.from, &f.to),
                (false, _) => myops::rename_table_sql(d, &f.schema, &f.from, &f.to, false),
            };
            match &built {
                Ok(sql) => sql_block(ui, &format!("{sql};")),
                Err(e) => {
                    sql_block(ui, if pg { "ALTER ... RENAME TO \"name\";" } else { "RENAME TABLE `old` TO `name`;" });
                    if f.to != f.from {
                        field_note(ui, e.as_str(), ALERT());
                    }
                }
            }
            if taken {
                field_note(ui, &format!("{} already exists here.", f.to), ALERT());
            }
            ui.add_space(8.0);
            let dependents = if pg {
                format!("Views, foreign keys and indexes follow the new name. Routines, trigger functions and queries that write {} are not updated and fail until they are edited.", f.from)
            } else if f.kind == DropKind::View {
                format!("Views, routines, triggers and events that name {} are not updated and fail until they are edited.", f.from)
            } else {
                format!("Views, routines, triggers and events that name {} are not updated and fail until they are edited. Foreign keys follow.", f.from)
            };
            let tabs = self.tabs_on(f.conn, &f.database, &f.schema, if f.kind == DropKind::Schema { "" } else { &f.from });
            banner(ui, Notice::Warn, "", &dependents, None);
            if self.staged_on(&tabs, f.conn, if f.kind == DropKind::Schema { "" } else { &f.from }) {
                ui.add_space(8.0);
                banner(ui, Notice::Warn, "", "A tab on it holds staged changes. Save or discard them first, then rename.", None);
                return (built, true);
            }
            (built, false)
        });
        let (built, staged) = built;
        dialog_foot(ui, |ui| {
            let ok = writable && built.is_ok() && f.to != f.from && !taken && !staged;
            if ui.add_enabled(ok, primary_button(ui, "Rename", PRIMARY())).clicked() {
                *keep = false;
                self.sql_rename_run(f);
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if !writable {
                foot_note(ui, NOTE_WRITE);
            }
        });
    }

    fn sql_rename_run(&mut self, f: &RenameForm) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(f.conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(f.conn, None));
        }
        let Some(sql) = self.session_of(f.conn, &f.database) else { return self.warn_offline() };
        let ro = !self.conn_writable(f.conn);
        self.worker.sql_rename(f.conn, sql, f.schema.clone(), f.from.clone(), f.to.clone(), f.kind == DropKind::Schema, ro);
    }

    /// A rename or its undo finished: the tabs on the old name take the new one, what the sidebar lists is read again, and the undo is kept.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn sql_renamed(&mut self, conn: u64, database: String, schema: String, from: String, to: String, whole: bool, res: db::Res<String>, undo: Option<UndoAction>) {
        match res {
            Ok(s) => self.ok(s),
            Err(e) => {
                // A refused undo hands its entry back, so it stays in the list with the reason.
                if let Some(u) = undo {
                    self.push_undo(u);
                }
                return self.err(e);
            }
        }
        // Row edits recorded under the old name would act on whatever is made under it next.
        self.purge_undo(conn, &database, &schema, if whole { "" } else { &from });
        if let Some(u) = undo {
            self.push_undo(u);
        }
        let tabs = self.tabs_on(conn, &database, &schema, if whole { "" } else { &from });
        for &i in &tabs {
            if whole {
                if !self.tabs[i].sel.schema.is_empty() {
                    self.tabs[i].sel.schema = to.clone();
                }
            } else {
                self.tabs[i].sel.coll = to.clone();
            }
        }
        if whole && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            if let Some(p) = c.pg.get_mut(&database).filter(|p| p.picked == schema) {
                p.picked = to.clone();
            }
            if let Some(t) = c.tables.get_mut(&database) {
                t.remove(&schema);
            }
        }
        if whole {
            self.forget_schema(conn, &ns_key(&database, &schema));
            self.pg_tables_asked.forget(conn, Some(&database), Some(&schema));
        }
        for i in tabs {
            self.reload_of(i);
        }
        if self.is_pg(conn) {
            self.pg_refresh(conn, &database);
        } else {
            self.side_act(conn, SideAct::Refresh(schema));
        }
    }

    pub(in crate::app) fn sql_duplicate_dialog(&mut self, ui: &mut egui::Ui, f: &mut DupForm, keep: &mut bool) {
        ui.set_width(540.0);
        let writable = self.conn_writable(f.conn);
        let (pg, d) = (self.is_pg(f.conn), self.dialect_of(f.conn));
        let place = if pg { format!("in {}.{} ", f.database, f.schema) } else { format!("in {} ", f.schema) };
        dialog_head(ui, &Head::new(format!("Duplicate {}", f.from)).sub(format!("{place}on {}", self.conn_name(f.conn))).glyph(glyphs::copy));
        let sel = Sel { conn: f.conn, db: if pg { f.database.clone() } else { f.schema.clone() }, schema: if pg { f.schema.clone() } else { String::new() }, coll: String::new() };
        let taken = self.tables_of(&sel).iter().any(|t| *t == f.to);
        let built = dialog_body(ui, |ui| {
            stacked_field(ui, "New table name", "", |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut f.to).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "New table name"));
                if !f.focused {
                    r.request_focus();
                    f.focused = true;
                }
            });
            if taken {
                field_note(ui, &format!("{} already exists here.", f.to), ALERT());
            }
            ui.add_space(10.0);
            card_caption(ui, "What to copy", "");
            let w = ui.available_width();
            if radio_card(ui, w, !f.data, "Structure only", "Columns, defaults, indexes and checks, and no rows").clicked() {
                f.data = false;
            }
            ui.add_space(6.0);
            if radio_card(ui, w, f.data, "Structure and data", "Also copies every row, in one step").clicked() {
                f.data = true;
            }
            ui.add_space(10.0);
            // Both statements as they run; the column list of the second is read from the server, so the preview shows `*`.
            let built = if pg {
                pgops::duplicate_stmts(&f.schema, &f.from, &f.to, f.data.then(|| [pgops::CopyCol { name: "*".into(), identity: None }]).as_ref().map(|c| &c[..]))
            } else {
                myops::duplicate_stmts(d, &f.schema, &f.from, &f.to, f.data.then(|| ["*".to_string()]).as_ref().map(|c| &c[..]), false)
            };
            match &built {
                Ok(v) => sql_block(ui, &v.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n").replace("\"*\"", "*").replace("`*`", "*")),
                Err(e) => {
                    sql_block(ui, "CREATE TABLE ...;");
                    if f.to != f.from && !f.to.is_empty() {
                        field_note(ui, e.as_str(), ALERT());
                    }
                }
            }
            ui.add_space(8.0);
            let note = if pg {
                "Foreign keys and triggers are not copied. Indexes, checks and column defaults are. Identity columns get a sequence of their own; a serial column keeps using the original's."
            } else {
                "Foreign keys and triggers are not copied. Indexes and column defaults are."
            };
            banner(ui, Notice::Info, "", note, None);
            if f.data {
                ui.add_space(8.0);
                banner(ui, Notice::Info, "", "A copy with rows has no undo, because it is never empty. Drop it from its menu if it is not wanted.", None);
            }
            built
        });
        dialog_foot(ui, |ui| {
            if ui.add_enabled(writable && built.is_ok() && !taken, primary_button(ui, "Duplicate", PRIMARY())).clicked() {
                *keep = false;
                self.sql_duplicate_run(f);
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if !writable {
                foot_note(ui, NOTE_WRITE);
            }
        });
    }

    fn sql_duplicate_run(&mut self, f: &DupForm) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(f.conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(f.conn, None));
        }
        let Some(sql) = self.session_of(f.conn, &f.database) else { return self.warn_offline() };
        let ro = !self.conn_writable(f.conn);
        self.worker.sql_duplicate(f.conn, sql, f.schema.clone(), f.from.clone(), f.to.clone(), f.data, ro);
    }

    pub(in crate::app) fn pg_truncate_dialog(&mut self, ui: &mut egui::Ui, f: &mut TruncForm, keep: &mut bool) {
        ui.set_width(540.0);
        let writable = self.conn_writable(f.conn);
        let conn = self.conn_name(f.conn);
        let tag = self.conn(f.conn).map(|c| c.meta.tag.clone()).unwrap_or_default();
        dialog_head(ui, &Head::new(format!("Empty table {}?", f.table)).sub(conn.clone()).glyph(glyphs::trash).tone(ALERT()));
        let ok = dialog_body(ui, |ui| {
            let wrap = |ui: &mut egui::Ui, text: String, color: Color32| {
                ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
            };
            let (mut loaded, mut blocked) = (false, false);
            match f.info.clone() {
                None => {
                    ui.horizontal(|ui| {
                        spin(ui);
                        ui.label(RichText::new("Counting rows…").color(MUTED()));
                    });
                }
                Some(Err(e)) => wrap(ui, format!("Could not look up what this holds: {e}"), ALERT()),
                Some(Ok(info)) if !info.exists => wrap(ui, format!("The table {} is not on {conn} any more.", f.table), ALERT()),
                Some(Ok(info)) => {
                    loaded = true;
                    wrap(ui, trunc_lead(&format!("{}.{}.{}", f.database, f.schema, f.table), &conn, &info), TEXT());
                    ui.add_space(4.0);
                    ui.checkbox(&mut f.restart, "Restart the sequences of its columns (RESTART IDENTITY)").on_hover_text("Identity and serial columns of the table count from their start again. Without it they carry on.");
                    if !info.referenced_by.is_empty() {
                        let n = info.referenced_by.len() as u64;
                        wrap(ui, format!("{} {} it with a foreign key: {}.", plural(n, "table"), if n == 1 { "points at" } else { "point at" }, list_and(&info.referenced_by)), MUTED());
                        ui.add_space(4.0);
                        ui.checkbox(&mut f.cascade, "Empty them too (CASCADE)");
                        if f.cascade {
                            wrap(ui, "CASCADE deletes every row of each table listed, and of any table that points at those.".into(), WARNING());
                        } else {
                            blocked = true;
                            wrap(ui, "PostgreSQL refuses to empty it while they point at it. Tick CASCADE to empty them as well.".into(), MUTED());
                        }
                    }
                }
            }
            ui.add_space(8.0);
            match pgops::truncate_sql(&f.schema, &f.table, f.restart, f.cascade) {
                Ok(sql) => sql_block(ui, &format!("{sql};")),
                Err(e) => wrap(ui, e, ALERT()),
            }
            ui.add_space(12.0);
            if !tag.is_empty() {
                banner(ui, Notice::Warn, "", &format!("This connection is tagged {tag}."), None);
            }
            crate::app::modals::typed_confirm(ui, &f.table, &mut f.typed);
            writable && loaded && !blocked && f.typed == f.table
        });
        dialog_foot(ui, |ui| {
            if danger_solid(ui, ok, "Empty table").clicked() {
                *keep = false;
                self.pg_truncate_run(f);
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if !writable {
                foot_note(ui, NOTE_WRITE);
            }
        });
    }

    fn pg_truncate_run(&mut self, f: &TruncForm) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(f.conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(f.conn, None));
        }
        let Some(sql) = self.session_of(f.conn, &f.database) else { return self.warn_offline() };
        let ro = !self.conn_writable(f.conn);
        self.worker.pg_truncate(f.conn, sql, f.schema.clone(), f.table.clone(), f.restart, f.cascade, ro);
    }

    /// What an emptied table leaves open: tabs on a table of the database read it again, because CASCADE reaches tables the menu did not name.
    pub(in crate::app) fn pg_emptied(&mut self, conn: u64, database: &str) {
        let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.conn == conn && self.tabs[i].sel.db == database && self.tabs[i].sql.is_some() && !self.tabs[i].sel.coll.is_empty()).collect();
        for i in open {
            self.reload_of(i);
        }
        self.pg_refresh(conn, database);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_lead_never_calls_an_estimate_exact() {
        let info = |rows, estimated| DropInfo { exists: true, rows, rows_estimated: estimated, ..Default::default() };
        assert!(trunc_lead("t", "c", &info(Some(3), false)).contains("(3 rows)"));
        assert!(trunc_lead("t", "c", &info(Some(1000), true)).contains("(about 1,000 rows)"));
        assert!(!trunc_lead("t", "c", &info(Some(0), true)).contains("0 rows"), "an estimate of 0 says nothing");
        assert!(!trunc_lead("t", "c", &info(None, true)).contains("rows)"));
    }
}
