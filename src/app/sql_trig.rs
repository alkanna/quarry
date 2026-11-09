//! Triggers of a SQL table: the editor under the Triggers grid, saving, dropping and the demo. The statements
//! come from `db::sql::ddl`; the preview shows exactly what Save sends, one statement per call. The body is the
//! user's own SQL, so nothing else is ever written into it.

use super::sql_alter::{sql_block, READ_ONLY_NOTE};
use super::*;
use db::sql::ddl::{self, TrigDef};
use db::sql::structure::{Structure, Trigger};

const LINE_H: f32 = 20.0;
const GUTTER_W: f32 = 52.0;

/// The key of the draft of a trigger that does not exist yet.
pub(super) const NEW: &str = "";

pub(super) fn def_of(t: &Trigger) -> TrigDef {
    TrigDef { name: t.name.clone(), timing: t.timing.to_ascii_uppercase(), event: t.event.to_ascii_uppercase(), body: t.body.clone() }
}

pub(super) fn blank() -> TrigDef {
    TrigDef { name: String::new(), timing: "BEFORE".into(), event: "INSERT".into(), body: "BEGIN\n  \nEND".into() }
}

/// A body is the same when only its outer whitespace differs, which the server does not keep.
pub(super) fn same(a: &TrigDef, b: &TrigDef) -> bool {
    a.name == b.name && a.timing == b.timing && a.event == b.event && a.body.trim() == b.body.trim()
}

/// The SQL text area with line numbers and token colors. Returns whether the text changed.
fn body_editor(ui: &mut egui::Ui, salt: &str, text: &mut String, height: f32, editable: bool) -> bool {
    let id = egui::Id::unique(("trigger-body", salt));
    let focused = ui.memory(|m| m.has_focus(id));
    let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = sql_job(buf.as_str(), TXT_SMALL);
        for sec in &mut job.sections {
            sec.format.line_height = Some(LINE_H);
        }
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let mut changed = false;
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt(("trigger-body-scroll", salt)).max_height(height).auto_shrink([false, true]).show(ui, |ui| {
            let out = egui::TextEdit::multiline(text)
                .id(id)
                .font(FontId::monospace(TXT_SMALL))
                .frame(egui::Frame::new().inner_margin(egui::Margin { left: GUTTER_W as i8, right: 12, top: 10, bottom: 10 }))
                .desired_width(f32::INFINITY)
                .min_size(egui::vec2(0.0, 96.0))
                .interactive(editable)
                .lock_focus(true)
                .layouter(&mut layout)
                .show(ui);
            changed = out.response.changed();
            let (g, gp, rect) = (&out.galley, out.galley_pos, out.response.rect);
            let (mut n, mut fresh) = (0, true);
            for row in &g.rows {
                if fresh {
                    n += 1;
                    let y = gp.y + row.pos.y + row.height() / 2.0;
                    ui.painter().text(egui::pos2(rect.left() + GUTTER_W - 14.0, y), egui::Align2::RIGHT_CENTER, n.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
                }
                fresh = row.ends_with_newline;
            }
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
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.st.trig_new = true;
            s.st.trig.entry(NEW.to_string()).or_insert_with(blank);
        }
    }

    /// Asks before dropping the selected trigger.
    pub(super) fn trig_ask_drop(&mut self, name: String) {
        if !self.sel_writable() {
            return self.err("This connection is read-only. Unlock writes in the top bar first.");
        }
        self.modal = Some(Modal::Typed { title: format!("Drop the trigger {name}? Undo brings it back as it was."), expect: name.clone(), typed: String::new(), action: Danger::DropTrigger(name) });
    }

    /// The editor, its preview and the Save row for `key`.
    pub(super) fn trig_editor(&mut self, ui: &mut egui::Ui, st: &Structure, key: &str, editable: bool) {
        let Some(table) = self.t().map(|t| t.sel.coll.clone()) else { return };
        let live: Option<TrigDef> = st.triggers.iter().find(|t| t.name == key).map(def_of);
        let (d, _, m) = self.alter_env();
        let busy = self.trig_state().is_some_and(|s| s.applying);
        let Some(mut def) = self.trig_state().and_then(|s| s.trig.get(key).cloned()).or_else(|| live.clone()).or_else(|| (key == NEW).then(blank)) else { return };
        let exists = live.is_some();
        let dirty = live.as_ref().is_none_or(|l| !same(l, &def));
        let was = def.clone();

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(RichText::new("Name").color(MUTED()));
            ui.add_enabled(editable && !exists, egui::TextEdit::singleline(&mut def.name).font(egui::TextStyle::Monospace).desired_width(240.0).hint_text("orders_before_update"));
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
        ui.add_space(8.0);
        let steps = if dirty { Some(ddl::trigger_steps(d, m.bs, m.ansi, &table, &def, exists)) } else { None };
        let reserve = if dirty { 250.0 } else { 70.0 };
        body_editor(ui, key, &mut def.body, (ui.available_height() - reserve).clamp(120.0, 700.0), editable);
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            if def != was || (dirty && !s.st.trig.contains_key(key)) {
                s.st.trig.insert(key.to_string(), def.clone());
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
                "Read-only: unlock writes to change triggers".to_string()
            } else if d == db::sql::Dialect::MySql {
                "Changing a trigger drops it and creates it again. If the new one is refused, the old one is put back.".to_string()
            } else {
                "Changing a trigger replaces it in one statement.".to_string()
            };
            ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
            right_group(ui, |ui| {
                let ok = editable && dirty && !busy && steps.as_ref().is_some_and(|s| s.is_ok());
                let why = if editable { "Nothing to save yet" } else { READ_ONLY_NOTE };
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
        if save {
            if let Some(Ok(steps)) = steps {
                self.run_trigger(def.name.clone(), steps, exists, false, if exists { "Saved the trigger" } else { "Created the trigger" });
            }
        }
    }

    pub(super) fn run_trigger(&mut self, name: String, steps: Vec<String>, existed: bool, dropping: bool, done: &str) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        if !self.sel_writable() {
            return self.err("This connection is read-only. Unlock writes in the top bar first.");
        }
        let Some((conn, schema, table)) = self.t().map(|t| (t.sel.conn, t.sel.db.clone(), t.sel.coll.clone())) else { return };
        let Some(db) = self.conn(conn).and_then(|c| c.sql.clone()) else { return self.err("Not connected.") };
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            s.st.applying = true;
        }
        self.worker.sql_trigger(conn, db, schema, table, name, steps, existed, dropping, false, done.to_string());
    }

    pub(super) fn drop_trigger(&mut self, name: String) {
        let d = self.alter_env().0;
        self.run_trigger(name.clone(), vec![ddl::drop_trigger(d, &name, false)], true, true, "Dropped the trigger");
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
            s.st.trig.insert("orders_before_update".into(), TrigDef { name: "orders_before_update".into(), timing: "BEFORE".into(), event: "UPDATE".into(), body: body.into() });
        }
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.writable = true;
        }
    }
}
