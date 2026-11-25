//! The New view and Edit view dialog of SQL connections, and the answers of its reads and saves. The statements come from
//! `db::sql::view`. The SELECT is a highlighted editor without suggestions, since the autocomplete of the Query view needs that
//! view's schema reads and state.

use crate::app::sql::create::{pick, sql_block};
use crate::app::*;
use db::sql::view::{self, ViewDef, ViewDone};
use quarry_core::action::DdlA;
pub(crate) use quarry_core::state::sql::viewdef::*;

pub(in crate::app) const NOTE_VIEW: &str = "Read-only: unlock writes to save a view";

/// A combo box over `(value, label)` items that writes the picked value.
fn choose(ui: &mut egui::Ui, id: &str, value: &mut String, items: &[(&str, &str)]) {
    let shown = items.iter().find(|(v, _)| v.eq_ignore_ascii_case(value)).map_or(value.clone(), |(_, l)| l.to_string());
    pick(ui, id, ui.available_width(), &shown, |ui| {
        for (v, l) in items {
            if ui.selectable_label(v.eq_ignore_ascii_case(value), *l).clicked() {
                *value = v.to_string();
            }
        }
    });
}

fn view_editor(ui: &mut egui::Ui, text: &mut dyn egui::TextBuffer, d: db::sql::Dialect, readonly: bool) {
    let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| crate::ui::editor::memo_galley(ui, 2, buf.as_str(), wrap, || crate::ui::editor::sql_job_in(buf.as_str(), TXT_SMALL, d));
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("view-sql").max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
            let r = crate::ui::editor::show_paired(ui, egui::TextEdit::multiline(text).font(FontId::monospace(TXT_SMALL)).frame(egui::Frame::NONE.inner_margin(egui::Margin::same(10))).desired_width(f32::INFINITY).desired_rows(9).lock_focus(true).interactive(!readonly).hint_text(RichText::new("SELECT * FROM orders").font(FontId::monospace(TXT_SMALL)).color(FAINT())).layouter(&mut layout));
            r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "View SELECT"));
        });
    });
}

impl App {
    /// Content tab: a new view from the SELECT of the grid's last page, not from what the bar says now.
    fn save_content_as_view(&mut self, i: usize) {
        let Some(t) = self.core.tabs.get(i) else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let (conn, db, coll) = (t.sel.conn, t.sel.db.clone(), t.sel.coll.clone());
        let body = s.ran.select_sql(self.dialect_of(conn), t.sel.space(), &coll, &[], None);
        self.exec(Action::Ddl(DdlA::NewView { conn, db, body }));
        if let Some(Modal::SqlView(f)) = self.modal.as_mut() {
            f.name = format!("{coll}_view");
        }
    }

    /// Opens the dialog a Content tab's Save as view button asked for. Called once a frame, since the button is drawn where the app is borrowed.
    pub(in crate::app) fn view_requests(&mut self) {
        let Some(i) = self.core.tabs.iter().position(|t| t.sql.as_ref().is_some_and(|s| s.save_view)) else { return };
        if let Some(s) = self.core.tabs[i].sql.as_mut() {
            s.save_view = false;
        }
        self.save_content_as_view(i);
    }

    pub(in crate::app) fn sql_view_def_done(&mut self, conn: u64, database: String, schema: String, name: String, res: db::Res<ViewDef>) {
        let Some(Modal::SqlView(f)) = self.modal.as_mut() else { return };
        if f.conn != conn || f.database != database || f.schema != schema || f.name != name || !f.loading {
            return;
        }
        f.loading = false;
        match res {
            Ok(d) => {
                f.body = d.body.clone();
                f.algorithm = d.algorithm.clone();
                f.security = d.security.clone();
                f.check = if d.check.is_empty() { "NONE".into() } else { d.check.clone() };
                f.keep_definer = d.definer.is_some();
                f.def = d;
            }
            Err(e) => f.error = Some(e),
        }
    }

    /// A save or an undo of a view finished. The dialog that sent it, while it waits, takes a failure itself and closes
    /// on success; the rest is Core's.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn sql_view_saved(&mut self, conn: u64, database: String, schema: String, name: String, kind: ViewDone, res: db::Res<String>, undo: Option<UndoAction>, need_drop: bool, sql_mode: Option<String>) {
        let waiting = matches!(&self.modal, Some(Modal::SqlView(f)) if f.conn == conn && f.database == database && f.schema == schema && f.busy);
        let ok = res.is_ok();
        if ok && waiting {
            self.modal = None;
        }
        if let Some(e) = self.core.sql_view_saved(conn, database, schema, name, kind, res, undo, sql_mode, waiting) && let Some(Modal::SqlView(f)) = self.modal.as_mut() {
            f.busy = false;
            if need_drop {
                f.ask_drop = true;
            } else {
                f.error = Some(e);
            }
        }
    }

    pub(in crate::app) fn sql_view_dialog(&mut self, ui: &mut egui::Ui, f: &mut ViewForm, keep: &mut bool) {
        ui.set_width(720.0);
        let writable = self.conn_writable(f.conn);
        let pg = self.is_pg(f.conn);
        let d = self.dialect_of(f.conn);
        let place = if pg { format!("{}.{}", f.database, f.schema) } else { f.schema.clone() };
        let head = Head::new(if f.edit { "Edit view" } else { "New view" }).sub(format!("{}{place} on {}", if f.edit { format!("{} in ", f.name) } else { String::new() }, self.conn_name(f.conn))).glyph(if f.edit { glyphs::pencil } else { glyphs::table });
        dialog_head(ui, &head);
        let materialized = f.def.materialized;
        let spec = f.spec(pg, false);
        let built = view::create_view_sql(d, &spec);
        dialog_body(ui, |ui| {
            if f.loading {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Reading the definition…").color(MUTED()));
                });
                return;
            }
            if materialized {
                banner(ui, Notice::Info, "Materialized view", "PostgreSQL has no CREATE OR REPLACE for a materialized view, so it cannot be edited here. The definition is shown as it is. To change it, drop the view and create it again.", None);
                ui.add_space(8.0);
            }
            if !f.edit || !f.def.name.is_empty() {
                form_card(ui, "", "", |rows| {
                    rows.label_width(110.0);
                    rows.row("Name", "", |ui| {
                        if f.edit {
                            ui.label(RichText::new(&f.name).font(FontId::monospace(TXT_BODY)).color(BRIGHT()));
                        } else {
                            let r = ui.add(egui::TextEdit::singleline(&mut f.name).font(egui::TextStyle::Monospace).desired_width(ui.available_width()));
                            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "View name"));
                            if !f.focused {
                                r.request_focus();
                                f.focused = true;
                            }
                        }
                    });
                    if !pg {
                        rows.row("Algorithm", "", |ui| choose(ui, "view-alg", &mut f.algorithm, &[("UNDEFINED", "Undefined (the server picks)"), ("MERGE", "Merge"), ("TEMPTABLE", "Temporary table")]));
                        rows.row("SQL security", "", |ui| choose(ui, "view-sec", &mut f.security, &[("DEFINER", "Definer (runs with the definer's rights)"), ("INVOKER", "Invoker (runs with the caller's rights)")]));
                        rows.row("Check option", "", |ui| choose(ui, "view-chk", &mut f.check, &[("NONE", "None"), ("CASCADED", "Cascaded"), ("LOCAL", "Local")]));
                        if let Some((u, h)) = f.def.definer.clone().filter(|_| f.edit) {
                            rows.row("Definer", "", |ui| {
                                toggle(ui, &mut f.keep_definer);
                                ui.label(RichText::new(if f.keep_definer { format!("Keep {u}@{h}") } else { "Run as this login".to_string() }).size(TXT_SMALL).color(MUTED()));
                            });
                        }
                    } else if !f.def.options.is_empty() {
                        rows.row("Options", "", |ui| {
                            ui.label(RichText::new(f.def.options.join(", ")).font(FontId::monospace(TXT_SMALL)).color(MUTED()));
                        });
                    }
                });
            }
            card_caption(ui, "SELECT", if materialized { "read-only" } else { "the rows the view shows" });
            if materialized {
                let mut shown = f.body.as_str();
                view_editor(ui, &mut shown, d, true);
            } else {
                view_editor(ui, &mut f.body, d, false);
            }
            ui.add_space(8.0);
            if let Some(e) = &f.error {
                banner(ui, Notice::Danger, if f.edit && f.def.name.is_empty() { "Could not read the view" } else { "Not saved" }, e, None);
                ui.add_space(8.0);
            }
            if f.ask_drop {
                banner(ui, Notice::Warn, "The columns changed", "CREATE OR REPLACE VIEW cannot change a view's columns. Drop the view and create it again instead? Grants on the view are lost. The drop is refused while other views depend on it, and then nothing changes.", None);
                ui.add_space(8.0);
            }
            if !materialized {
                match &built {
                    Ok(sql) => sql_block(ui, &format!("{sql};")),
                    Err(e) => {
                        // An empty form is not a mistake yet.
                        if !f.body.trim().is_empty() || !f.name.is_empty() {
                            field_note(ui, e.as_str(), ALERT());
                        }
                    }
                }
            }
        });
        let ok = writable && !f.loading && !f.busy && !materialized && (!f.edit || !f.def.name.is_empty()) && built.is_ok();
        dialog_foot(ui, |ui| {
            if f.ask_drop {
                if ui.add_enabled_ui(ok, |ui| danger(ui, "Drop and create")).inner.clicked() {
                    self.core.view_save(f, true);
                }
            } else if ui.add_enabled(ok, primary_button(ui, if f.edit { "Save view" } else { "Create view" }, PRIMARY())).clicked() {
                self.core.view_save(f, false);
            }
            if ui.button(if materialized { "Close" } else { "Cancel" }).on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if f.busy {
                spin(ui);
            }
            if !writable {
                foot_note(ui, NOTE_VIEW);
            }
        });
    }
}
