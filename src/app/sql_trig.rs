//! Triggers of a SQL table: the editor under the Triggers grid, saving, dropping and the demo. The statements
//! come from `db::sql::ddl`; the preview shows what Save sends. The body is the user's own SQL, so nothing else is
//! ever written into it, and it is checked to be exactly one statement before it is shown or sent.

use super::sql_alter::{sql_block, BUSY_NOTE};
use super::*;
use db::sql::ddl::{self, TrigDef, TrigJob};
use db::sql::structure::{Structure, Trigger};

const LINE_H: f32 = 20.0;

/// The key of the draft of a trigger that does not exist yet.
pub(super) const NEW: &str = "";

pub(super) fn def_of(t: &Trigger) -> TrigDef {
    if t.pg.is_some() {
        return db::sql::ddl_pg_trig::def_of(t);
    }
    TrigDef { name: t.name.clone(), timing: t.timing.to_ascii_uppercase(), event: t.event.to_ascii_uppercase(), body: t.body.clone(), pg: Default::default() }
}

/// The draft of a new trigger for the engine.
pub(super) fn blank(pg: bool) -> TrigDef {
    if pg {
        return db::sql::ddl_pg_trig::blank();
    }
    TrigDef { name: String::new(), timing: "BEFORE".into(), event: "INSERT".into(), body: "BEGIN\n  \nEND".into(), pg: Default::default() }
}

/// A body is the same when only its outer whitespace differs, which the server does not keep. The name of a new trigger's
/// function is not compared: a trigger that was just made has the name the server kept, and the draft may have left it to
/// be picked.
pub(super) fn same(a: &TrigDef, b: &TrigDef) -> bool {
    a.name == b.name && a.timing == b.timing && a.event == b.event && a.body.trim() == b.body.trim() && db::sql::ddl::PgDef { func: String::new(), ..a.pg.clone() } == db::sql::ddl::PgDef { func: String::new(), ..b.pg.clone() }
}

/// Drops the drafts of triggers that are gone, so a trigger made again later under the same name does not open
/// with the edits of the dropped one. The new trigger's draft stays.
pub(super) fn prune_drafts(drafts: &mut std::collections::HashMap<String, TrigDef>, live: &[Trigger]) {
    drafts.retain(|k, _| k == NEW || live.iter().any(|t| t.name == *k));
}

/// The SQL text area with line numbers and token colors. Returns whether the text changed.
fn body_editor(ui: &mut egui::Ui, salt: &str, text: &mut String, height: f32, editable: bool) -> bool {
    let id = egui::Id::unique(("trigger-body", salt));
    let focused = ui.memory(|m| m.has_focus(id));
    let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        crate::ui::editor::memo_galley(ui, 1, buf.as_str(), wrap, || {
            let mut job = sql_job(buf.as_str(), TXT_SMALL);
            for sec in &mut job.sections {
                sec.format.line_height = Some(LINE_H);
            }
            job
        })
    };
    let mut changed = false;
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt(("trigger-body-scroll", salt)).max_height(height).auto_shrink([false, true]).show(ui, |ui| {
            let out = egui::TextEdit::multiline(text)
                .id(id)
                .font(FontId::monospace(TXT_SMALL))
                .frame(gutter_frame())
                .desired_width(f32::INFINITY)
                .min_size(egui::vec2(0.0, 96.0))
                .interactive(editable)
                .lock_focus(true)
                .layouter(&mut layout)
                .show(ui);
            changed = out.response.changed();
            out.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, editable, "Trigger body"));
            paint_gutter(ui, &out, None);
        });
    });
    changed
}

impl App {
    fn trig_state(&self) -> Option<&super::sql_structure::StructState> {
        Some(&self.t()?.sql.as_ref()?.st)
    }

    /// The draft being edited: the new trigger's, or the selected trigger's.
    pub(super) fn trig_key(&self, st: &Structure, sel: Option<usize>) -> Option<String> {
        if self.trig_state()?.trig_new {
            return Some(NEW.to_string());
        }
        let i = sel.filter(|i| *i < st.triggers.len()).or(if st.triggers.is_empty() { None } else { Some(0) })?;
        Some(st.triggers[i].name.clone())
    }

    pub(super) fn trig_new(&mut self) {
        let pg = self.t().is_some_and(|t| self.is_pg(t.sel.conn));
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.st.trig_new = true;
            s.st.trig.entry(NEW.to_string()).or_insert_with(|| blank(pg));
        }
    }

    /// Whether the connection of the open table lets writes through, which a view's own lock on rows does not decide: a view
    /// can hold INSTEAD OF triggers on PostgreSQL.
    pub(super) fn tab_writable(&self) -> bool {
        self.t().and_then(|t| self.conn(t.sel.conn)).is_some_and(|c| c.writable)
    }

    /// Whether the triggers of the open table can be edited at all: the engine can, and the table is not a view that the
    /// engine cannot put a trigger on. A locked connection still edits; Save and Delete ask to unlock.
    pub(super) fn trig_editable(&self) -> bool {
        self.trig_block().is_none() && (!self.sel_is_view() || self.t().is_some_and(|t| self.is_pg(t.sel.conn)))
    }

    /// Asks before dropping the selected trigger.
    pub(super) fn trig_ask_drop(&mut self, name: String) {
        if !self.tab_writable() {
            return self.ask_unlock_then(move |a| a.trig_ask_drop(name));
        }
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let mut title = format!("Drop the trigger {name}? Undo brings it back as it was.");
        // PostgreSQL drops the function with its trigger when nothing else runs it, or says why it stays; a trigger that cannot
        // be put back is refused here, before the person types anything.
        if self.is_pg(sel.conn) {
            let Some(t) = self.structure().and_then(|s| s.triggers.iter().find(|t| t.name == name)).cloned() else { return self.warn(format!("The trigger {name} is not in the structure any more.")) };
            match db::sql::ddl_pg_trig::preview(&self.sel_schema(&sel), &sel.coll, &TrigJob::Drop(t)) {
                Ok((steps, warnings)) => {
                    if steps.len() > 1 {
                        title.push_str(" Its function is dropped with it, since nothing else runs it.");
                    }
                    for w in warnings {
                        title.push(' ');
                        title.push_str(&w);
                    }
                }
                Err(why) => return self.warn(why),
            }
        }
        self.modal = Some(Modal::Typed { title, expect: name.clone(), typed: String::new(), action: Danger::DropTrigger(sel, name) });
    }

    /// The editor, its preview and the Save row for `key`.
    pub(super) fn trig_editor(&mut self, ui: &mut egui::Ui, st: &Structure, key: &str, editable: bool) {
        let Some((conn, table)) = self.t().map(|t| (t.sel.conn, t.sel.coll.clone())) else { return };
        let live_trig: Option<Trigger> = st.triggers.iter().find(|t| t.name == key).cloned();
        let live: Option<TrigDef> = live_trig.as_ref().map(def_of);
        let (d, _, m) = self.alter_env();
        // None offline, where there is no session to compare with.
        let session_mode = self.conn(conn).and_then(|c| c.sql.as_ref()).map(|s| s.sql_mode.clone());
        let busy = self.trig_state().is_some_and(|s| s.applying);
        let pg = d == db::sql::Dialect::Postgres;
        let Some(mut def) = self.trig_state().and_then(|s| s.trig.get(key).cloned()).or_else(|| live.clone()).or_else(|| (key == NEW).then(|| blank(pg))) else { return };
        let exists = live.is_some();
        let dirty = live.as_ref().is_none_or(|l| !same(l, &def));
        let was = def.clone();
        // A trigger the server cannot have rewritten is shown but never editable, and the editor says why.
        let pg_live = live_trig.as_ref().and_then(|t| t.pg.as_deref());
        let editable = editable && pg_live.is_none_or(|p| p.refuse.is_empty());
        let cols: Vec<String> = st.columns.iter().map(|c| c.name.clone()).collect();

        if pg {
            super::sql_trig_pg::head(ui, &mut def, &super::sql_trig_pg::Env { exists, editable, live: pg_live, table: &table });
        } else {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new("Name").color(MUTED()));
                ui.add_enabled(editable && !exists, egui::TextEdit::singleline(&mut def.name).font(egui::TextStyle::Monospace).desired_width(240.0).hint_text("orders_before_update")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, editable && !exists, "Trigger name"));
                ui.add_space(8.0);
                ui.label(RichText::new("Timing").color(MUTED()));
                ui.add_enabled_ui(editable, |ui| {
                    let mut i = usize::from(def.timing == "AFTER");
                    segmented(ui, &mut i, &[(0, "BEFORE"), (1, "AFTER")]);
                    def.timing = ["BEFORE", "AFTER"][i].into();
                });
                ui.add_space(8.0);
                ui.label(RichText::new("Event").color(MUTED()));
                ui.add_enabled_ui(editable, |ui| {
                    let mut i = ["INSERT", "UPDATE", "DELETE"].iter().position(|e| *e == def.event).unwrap_or(0);
                    segmented(ui, &mut i, &[(0, "INSERT"), (1, "UPDATE"), (2, "DELETE")]);
                    def.event = ["INSERT", "UPDATE", "DELETE"][i].into();
                });
            });
        }
        ui.add_space(8.0);
        let reserve = if dirty { 270.0 } else { 70.0 };
        // A function in another language is shown, not rewritten, since only plpgsql can be written back exactly.
        let body_editable = editable && pg_live.is_none_or(|p| p.lang == "plpgsql");
        body_editor(ui, key, &mut def.body, (ui.available_height() - reserve).clamp(120.0, 700.0), body_editable);
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            if def != was || (dirty && !s.st.trig.contains_key(key)) {
                s.st.trig.insert(key.to_string(), def.clone());
            }
        }
        let dirty = live.as_ref().is_none_or(|l| !same(l, &def));
        let job = ddl::save_job(&st.triggers, live_trig.as_ref(), &def);
        let mut pg_warnings: Vec<String> = Vec::new();
        let schema = self.t().map(|t| self.sel_schema(&t.sel)).unwrap_or_default();
        let steps = dirty.then(|| {
            if !pg {
                return ddl::trigger_preview(d, m, &table, &job);
            }
            if let Some(why) = super::sql_trig_pg::column_problem(&def, &cols) {
                return Err(why);
            }
            db::sql::ddl_pg_trig::preview(&schema, &table, &job).map(|(steps, warnings)| {
                pg_warnings = warnings;
                steps
            })
        });
        for w in &pg_warnings {
            ui.add_space(6.0);
            ui.label(RichText::new(w.as_str()).size(TXT_SMALL).color(WARNING()));
        }
        // The edited trigger is created with this session's sql_mode, which decides how its body runs.
        if let (Some(t), Some(now)) = (live_trig.as_ref().filter(|_| dirty), &session_mode) {
            if !ddl::same_modes(&t.sql_mode, now) {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{} was created with sql_mode {:?}. Saving creates it with this session's {:?}, which can change how its body runs (|| or double quotes, for example).", t.name, t.sql_mode, now)).size(TXT_SMALL).color(WARNING()));
            }
        }
        if let Some(steps) = &steps {
            ui.add_space(8.0);
            caption(ui, "SQL TO RUN");
            ui.add_space(4.0);
            match steps {
                Ok(s) => sql_block(ui, "trigger-sql", &ddl::script(s), 120.0),
                Err(e) => {
                    ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(if def.name.is_empty() { FAINT() } else { ALERT() }));
                }
            }
        }
        ui.add_space(8.0);
        let (mut save, mut revert) = (false, false);
        ui.horizontal(|ui| {
            let note = if !editable {
                "This trigger cannot be changed here"
            } else if !self.tab_writable() {
                "Writes are locked: Save asks to unlock them"
            } else if pg {
                "One transaction: the function is replaced, then the trigger is dropped and created again. If anything is refused, nothing changes."
            } else if matches!(d, db::sql::Dialect::MySql) {
                "MySQL tries the new trigger under a scratch name, then swaps it in while the table is locked. If it is refused, the old one stays."
            } else {
                "Changing a trigger replaces it in one statement."
            };
            // Room is kept for Revert and Save, so a long note is cut instead of running under them.
            let w = (ui.available_width() - 230.0).max(60.0);
            ui.allocate_ui_with_layout(egui::vec2(w, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(note).size(TXT_SMALL).color(FAINT())).truncate()).on_hover_text(note);
            });
            right_group(ui, |ui| {
                let ok = editable && dirty && !busy && steps.as_ref().is_some_and(|s| s.is_ok());
                let why = if !editable { "This trigger cannot be changed here" } else if busy { BUSY_NOTE } else { "Nothing to save yet" };
                save = ui.add_enabled(ok, primary_button(ui, if busy { "Saving..." } else { "Save trigger" }, PRIMARY())).on_disabled_hover_text(why).clicked();
                if dirty && quiet_enabled(ui, !busy, "Revert").on_hover_text("Discard the edits").clicked() {
                    revert = true;
                }
            });
        });
        if revert {
            if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
                s.st.trig.remove(key);
                if key == NEW {
                    s.st.trig_new = false;
                }
            }
        }
        if save && steps.as_ref().is_some_and(|s| s.is_ok()) {
            self.run_trigger(job, if exists { "Saved the trigger" } else { "Created the trigger" });
        }
    }

    pub(super) fn run_trigger(&mut self, job: TrigJob, done: &str) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        // A locked connection asks first and runs the change when writes are allowed, as Apply does for the structure.
        if !self.tab_writable() {
            let done = done.to_string();
            return self.ask_unlock_then(move |a| a.run_trigger(job, &done));
        }
        if self.applying() {
            return self.warn(BUSY_NOTE);
        }
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let Some((db, schema)) = self.sql_target(&sel) else { return self.warn_offline() };
        let (.., m) = self.alter_env();
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            s.st.applying = true;
            // The draft of a trigger that is being made is cleared once the structure lists it under this name.
            s.st.trig_made = if let TrigJob::Create(c) = &job { Some(c.def.name.clone()) } else { None };
        }
        self.worker.sql_trigger(sel.conn, db, schema, sel.coll, job, m, false, done.to_string());
    }

    pub(super) fn drop_trigger(&mut self, name: String) {
        let Some(t) = self.structure().and_then(|s| s.triggers.iter().find(|t| t.name == name)).cloned() else { return self.warn(format!("The trigger {name} is not in the structure any more.")) };
        self.run_trigger(TrigJob::Drop(t), "Dropped the trigger");
    }

    /// QUARRY_TEST_SQL=trigedit: the orders_before_update trigger with its body changed, so the preview shows
    /// the DROP and CREATE that Save would send. The demo connection is unlocked so Save is live.
    pub(super) fn sql_trig_demo(&mut self, which: &str) {
        if which != "trigedit" {
            return;
        }
        let body = "BEGIN\n  IF NEW.status = 'refunded' AND OLD.status <> 'paid' THEN\n    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'Only paid orders can be refunded';\n  END IF;\n  IF NEW.total < 0 THEN\n    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'The total cannot be negative';\n  END IF;\nEND";
        let Some(t) = self.tabs.first_mut() else { return };
        let conn = t.sel.conn;
        if let Some(s) = t.sql.as_mut() {
            s.st.trig.insert("orders_before_update".into(), TrigDef { name: "orders_before_update".into(), timing: "BEFORE".into(), event: "UPDATE".into(), body: body.into(), pg: Default::default() });
        }
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.writable = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trig(name: &str, timing: &str, event: &str, order: u64) -> Trigger {
        Trigger { name: name.into(), timing: timing.into(), event: event.into(), body: "SET NEW.a = 1".into(), definer: "app@%".into(), order, sql_mode: String::new(), pg: None }
    }

    /// Review findings 5 and 6: an edit keeps the definer and the place among the triggers of its event.
    #[test]
    fn an_edit_keeps_its_definer_and_its_place() {
        let all = [trig("t1", "BEFORE", "INSERT", 1), trig("t2", "BEFORE", "INSERT", 2), trig("t3", "BEFORE", "INSERT", 3), trig("u1", "AFTER", "INSERT", 1)];
        let edit = |i: usize, timing: &str| match ddl::save_job(&all, Some(&all[i]), &TrigDef { timing: timing.into(), ..def_of(&all[i]) }) {
            TrigJob::Edit { save, .. } => save,
            _ => panic!("an edit"),
        };
        assert_eq!(edit(1, "BEFORE").order, Some(ddl::TrigOrder::Follows("t1".into())));
        assert_eq!(edit(0, "BEFORE").order, Some(ddl::TrigOrder::Precedes("t2".into())));
        assert_eq!(edit(3, "AFTER").order, None, "alone in its event");
        assert_eq!(edit(1, "AFTER").order, None, "a new timing goes last in its new group");
        assert_eq!(edit(1, "BEFORE").definer.as_deref(), Some("app@%"));
        assert!(matches!(ddl::save_job(&all, None, &blank(false)), TrigJob::Create(ddl::TrigSave { definer: None, order: None, .. })));
    }

    /// Review finding 14: the draft of a dropped trigger is dropped with it.
    #[test]
    fn a_dropped_trigger_takes_its_draft_along() {
        let mut drafts: std::collections::HashMap<String, TrigDef> = [("t1".to_string(), blank(false)), ("gone".to_string(), blank(false)), (NEW.to_string(), blank(false))].into_iter().collect();
        prune_drafts(&mut drafts, &[trig("t1", "BEFORE", "INSERT", 1)]);
        let mut keys: Vec<&String> = drafts.keys().collect();
        keys.sort();
        assert_eq!(keys, [NEW, "t1"]);
    }
}
