//! Renaming and duplicating SQL tables and views, renaming a PostgreSQL schema, and emptying a PostgreSQL table: the three dialogs and
//! what the app does when the server answers. The statements come from `db::sql::tableops` (MySQL, MariaDB) and
//! `db::sql::pg::tableops`. MySQL's own Empty table is the drop dialog's (`DropKind::Truncate`).

use crate::app::sql::create::{list_and, plural, sql_block, DropKind};
use crate::app::sql::ns::sel_of;
use crate::app::*;
use db::sql::create::DropInfo;
use db::sql::pg::tableops as pgops;
use quarry_core::action::DdlA;
pub(crate) use quarry_core::state::sql::tableops::*;

const NOTE_WRITE: &str = "Read-only: unlock writes to change tables";

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
        let taken = f.kind != DropKind::Schema && f.to != f.from && self.table_listed(&sel_of(f.conn, &f.database, &f.schema, String::new()), &f.to);
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
            let built = f.preview(pg, d);
            match &built {
                Ok(sql) => sql_block(ui, sql),
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
            let tabs = self.core.tabs_on(f.conn, &f.database, &f.schema, if f.kind == DropKind::Schema { "" } else { &f.from });
            banner(ui, Notice::Warn, "", &dependents, None);
            if self.core.staged_on(&tabs, f.conn, if f.kind == DropKind::Schema { "" } else { &f.from }) {
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
                self.exec(Action::Ddl(DdlA::RenameRun { conn: f.conn, database: f.database.clone(), schema: f.schema.clone(), from: f.from.clone(), to: f.to.clone(), kind: f.kind }));
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if !writable {
                foot_note(ui, NOTE_WRITE);
            }
        });
    }

    pub(in crate::app) fn sql_duplicate_dialog(&mut self, ui: &mut egui::Ui, f: &mut DupForm, keep: &mut bool) {
        ui.set_width(540.0);
        let writable = self.conn_writable(f.conn);
        let (pg, d) = (self.is_pg(f.conn), self.dialect_of(f.conn));
        let place = if pg { format!("in {}.{} ", f.database, f.schema) } else { format!("in {} ", f.schema) };
        dialog_head(ui, &Head::new(format!("Duplicate {}", f.from)).sub(format!("{place}on {}", self.conn_name(f.conn))).glyph(glyphs::copy));
        let taken = self.table_listed(&sel_of(f.conn, &f.database, &f.schema, String::new()), &f.to);
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
            let built = f.preview(pg, d);
            match &built {
                Ok(text) => sql_block(ui, text),
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
                self.exec(Action::Ddl(DdlA::DuplicateRun { conn: f.conn, database: f.database.clone(), schema: f.schema.clone(), from: f.from.clone(), to: f.to.clone(), data: f.data }));
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if !writable {
                foot_note(ui, NOTE_WRITE);
            }
        });
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
                self.exec(Action::Ddl(DdlA::TruncateRun { conn: f.conn, database: f.database.clone(), schema: f.schema.clone(), table: f.table.clone(), restart: f.restart, cascade: f.cascade }));
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if !writable {
                foot_note(ui, NOTE_WRITE);
            }
        });
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
