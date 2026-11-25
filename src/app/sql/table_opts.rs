//! The table options of a SQL table in Structure: comment, engine, default collation and the auto increment counter. Changes
//! are staged like column and index edits (`alter.rs`); each one carries the value the structure read, which is what its undo writes.

use crate::app::sql::alter::{sql_block, BUSY_NOTE};
use crate::app::sql::create::pick;
use crate::app::*;
use db::sql::ddl::{self, Change, TableOpt};
use db::sql::Dialect;
pub(crate) use quarry_core::state::sql::table_opts::*;

impl App {
    pub(in crate::app) fn open_table_opts(&mut self) {
        let Some(was) = self.structure().map(|st| (st.status.comment.clone(), st.status.engine.clone(), st.status.collation.clone(), st.status.auto_increment)) else { return };
        let Some((conn, schema)) = self.t().map(|t| (t.sel.conn, t.sel.db.clone())) else { return };
        let pg = self.alter_env().0 == Dialect::Postgres;
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
        self.modal = Some(Modal::TableOpts(Box::new(TableForm { conn, schema, pg, comment, engine, collation, auto, was, rest, had_staged, meta: None, load_err: None, view: None })));
    }

    /// One change for each field that differs from what the structure read.
    fn table_changes(f: &TableForm) -> Result<Vec<Change>, String> {
        let mut v = Vec::new();
        let (comment, engine, collation, auto) = &f.was;
        if f.comment != *comment {
            v.push(Change::Table { opt: TableOpt::Comment(f.comment.clone()), was: TableOpt::Comment(comment.clone()) });
        }
        if !f.pg {
            if f.engine != *engine && !f.engine.is_empty() {
                v.push(Change::Table { opt: TableOpt::Engine(f.engine.clone()), was: TableOpt::Engine(engine.clone()) });
            }
            if f.collation != *collation && !f.collation.is_empty() {
                v.push(Change::Table { opt: TableOpt::Collation(f.collation.clone()), was: TableOpt::Collation(collation.clone()) });
            }
            let typed = f.auto.trim();
            if let (Some(old), false) = (auto, typed.is_empty()) {
                let n: u64 = typed.parse().map_err(|_| "The next auto increment value is a whole number.".to_string())?;
                if n == 0 {
                    return Err("The next auto increment value starts at 1.".into());
                }
                if n != *old {
                    v.push(Change::Table { opt: TableOpt::AutoIncrement(n), was: TableOpt::AutoIncrement(*old) });
                }
            }
        }
        Ok(v)
    }

    fn table_view(&self, f: &TableForm) -> TableView {
        let key = [f.comment.clone(), f.engine.clone(), f.collation.clone(), f.auto.clone()];
        let (changes, err) = match Self::table_changes(f) {
            Ok(c) => (c, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let invalid = err.is_some();
        let removes = !invalid && changes.is_empty() && f.had_staged;
        let mut all = f.rest.clone();
        all.extend(changes.clone());
        let text = match err {
            Some(e) => Err(e),
            None if removes => Err("The staged table options are taken back.".to_string()),
            None if changes.is_empty() => Err("Nothing is changed yet.".to_string()),
            None => self.structure().map_or_else(|| Err("The structure is not loaded.".into()), |st| self.alter_text(st, &all)),
        };
        let warnings = self.structure().map(|st| ddl::warnings(st, &all)).unwrap_or_default();
        TableView { key, invalid, removes, new: changes, text, warnings }
    }

    pub(in crate::app) fn table_dialog(&mut self, ui: &mut egui::Ui, f: &mut TableForm, keep: &mut bool) {
        let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        ui.set_width(560.0);
        dialog_head(ui, &Head::new("Table options").sub(table).glyph(glyphs::table));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        form_grid(ui, "tableopts", |ui| {
            form_label(ui, "Comment");
            ui.add(egui::TextEdit::multiline(&mut f.comment).font(egui::TextStyle::Monospace).desired_rows(2).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Table comment"));
            ui.end_row();
            if !f.pg {
                form_label(ui, "Engine");
                match &f.meta {
                    Some(m) => {
                        let shown = f.engine.clone();
                        pick(ui, "tableopts-engine", 260.0, &shown, |ui| {
                            for e in &m.engines {
                                if ui.selectable_label(*e == f.engine, e).clicked() {
                                    f.engine = e.clone();
                                }
                            }
                        });
                    }
                    None => {
                        spin(ui);
                    }
                }
                ui.end_row();

                form_label(ui, "Collation");
                match &f.meta {
                    Some(m) => {
                        let shown = f.collation.clone();
                        pick(ui, "tableopts-co", 260.0, &shown, |ui| {
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                for c in &m.collations {
                                    if ui.selectable_label(c.name == f.collation, format!("{}  ({})", c.name, c.charset)).clicked() {
                                        f.collation = c.name.clone();
                                    }
                                }
                            });
                        });
                    }
                    None => {
                        spin(ui);
                    }
                }
                ui.end_row();

                form_label(ui, "Auto increment");
                if f.was.3.is_some() {
                    ui.add(egui::TextEdit::singleline(&mut f.auto).font(egui::TextStyle::Monospace).desired_width(160.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Next auto increment value"));
                } else {
                    ui.label(RichText::new("This table has no auto increment column.").size(TXT_SMALL).color(FAINT()));
                }
                ui.end_row();
            }
        });
        if let Some(e) = &f.load_err {
            ui.label(RichText::new(format!("Could not read the server's engines and collations: {e}")).size(TXT_SMALL).color(ALERT()));
        }

        // The plan walks the whole structure, so it runs when a field changes and not on every frame.
        // Compared in place, so a frame with nothing typed copies no text.
        if f.view.as_ref().is_none_or(|v| v.key[0] != f.comment || v.key[1] != f.engine || v.key[2] != f.collation || v.key[3] != f.auto) {
            f.view = Some(self.table_view(f));
        }
        let v = f.view.as_ref().unwrap();
        let (text, changes_empty, invalid, removes) = (&v.text, v.new.is_empty(), v.invalid, v.removes);
        ui.add_space(6.0);
        caption(ui, "SQL TO RUN");
        ui.add_space(4.0);
        match text {
            Ok(t) => sql_block(ui, "tableopts-sql", t, 110.0),
            Err(e) => {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(if changes_empty && !invalid { FAINT() } else { ALERT() }));
            }
        }
        if !f.pg && f.collation != f.was.2 {
            ui.label(RichText::new("Only the columns added later use the new collation. Existing columns keep theirs.").size(TXT_SMALL).color(WARNING()));
        }
        for w in &v.warnings {
            ui.label(RichText::new(w.as_str()).size(TXT_SMALL).color(WARNING()));
        }
        let text_ok = text.is_ok();
        let can_stage = text_ok || removes;
        let (mut stage, mut now) = (false, false);
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
            stage = ui.add_enabled(can_stage, primary_button(ui, if removes { "Take back" } else { "Stage change" }, PRIMARY())).clicked();
            let busy = self.applying();
            now = ui.add_enabled(text_ok && !self.sel_is_view() && !busy, egui::Button::new("Apply now")).on_hover_text("Stage these changes and run everything staged. A locked connection asks to unlock first.").on_disabled_hover_text(if busy { BUSY_NOTE } else { "A view cannot be changed here" }).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if stage || now {
            *keep = false;
            let new = f.view.take().map(|v| v.new).unwrap_or_default();
            // From the list as it is now (an Apply that finished meanwhile emptied it), with this dialog's options replacing the staged ones.
            let mut staged: Vec<Change> = self.staged().iter().filter(|c| !matches!(c, Change::Table { .. })).cloned().collect();
            staged.extend(new);
            self.set_staged(staged);
            if now {
                self.request_alter_apply();
            }
        }
    }
}
