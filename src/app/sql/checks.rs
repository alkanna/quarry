//! CHECK constraints of a SQL table: the Add and Edit check dialog and dropping one. Changes are staged like column and
//! index edits (`alter.rs`), so the statement the preview shows is the one that runs and the undo comes from the structure
//! read before. Editing a CHECK is a drop and an add staged together, since no engine changes the condition in place.

use crate::app::sql::alter::{restage, sql_block, BUSY_NOTE};
use crate::app::*;
use db::sql::ddl::{self, Change};
use db::sql::structure::CheckConstraint;
use db::sql::Dialect;

/// The Add check and Edit check dialog.
pub(in crate::app) struct CheckForm {
    /// The constraint being edited; None adds one.
    from: Option<CheckConstraint>,
    def: CheckConstraint,
    /// The staged changes this one goes after.
    rest: Vec<Change>,
    view: Option<CheckView>,
}

/// The plan and the warnings of the constraint as it stands, kept until the definition changes.
struct CheckView {
    def: CheckConstraint,
    changes_empty: bool,
    /// The staged changes with this one added, which Stage change keeps.
    all: Vec<Change>,
    text: Result<String, String>,
    warnings: Vec<String>,
}

impl App {
    pub(in crate::app) fn open_check_form(&mut self, i: Option<usize>) {
        let Some(st) = self.structure().cloned() else { return };
        let staged = self.staged().to_vec();
        let from = i.and_then(|i| st.checks.get(i)).cloned();
        if let Some(f) = &from && staged.iter().any(|c| matches!(c, Change::DropCheck(n) if *n == f.name)) {
            return self.warn(format!("{} already has a staged change. Remove it first.", f.name));
        }
        let def = match &from {
            Some(f) => f.clone(),
            None => {
                // A name to start from, so the statement reads at once and the person only has to type the condition.
                let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                let taken = ddl::checks_after(&st, &staged);
                let name = (0..).map(|n| if n == 0 { format!("chk_{table}") } else { format!("chk_{table}_{n}") }).find(|c| !taken.iter().any(|t| ddl::same_name(t, c))).unwrap_or_default();
                CheckConstraint { name, ..Default::default() }
            }
        };
        self.modal = Some(Modal::CheckEdit(Box::new(CheckForm { from, def, rest: staged, view: None })));
    }

    pub(in crate::app) fn stage_drop_check(&mut self, i: usize) {
        let Some(k) = self.structure().and_then(|s| s.checks.get(i)) else { return };
        let name = k.name.clone();
        let mut staged = self.staged().to_vec();
        if staged.iter().any(|c| matches!(c, Change::DropCheck(n) if *n == name)) {
            return;
        }
        staged.push(Change::DropCheck(name));
        self.set_staged(staged);
    }

    /// What the dialog shows for its current definition: the changes it stages, the SQL or the reason there is none, and the warnings.
    fn check_view(&self, f: &CheckForm) -> CheckView {
        let pg = self.alter_env().0 == Dialect::Postgres;
        let def = CheckConstraint { name: f.def.name.trim().to_string(), expr: f.def.expr.trim().to_string(), ..f.def.clone() };
        let changes: Vec<Change> = match &f.from {
            None => vec![Change::AddCheck(def.clone())],
            Some(old) if *old == def => Vec::new(),
            Some(old) => vec![Change::DropCheck(old.name.clone()), Change::AddCheck(def.clone())],
        };
        let mut all = f.rest.clone();
        all.extend(changes.clone());
        // Against the names the other staged changes leave, as the server compares them.
        let taken = self.structure().map(|s| ddl::checks_after(s, &f.rest)).unwrap_or_default();
        let dup = taken.iter().any(|x| ddl::fk_name_is(pg, x, &def.name) && f.from.as_ref().is_none_or(|o| !ddl::fk_name_is(pg, &o.name, x)));
        let text = if changes.is_empty() {
            Err("Nothing is changed yet.".to_string())
        } else if def.name.is_empty() {
            Err("A check constraint needs a name.".to_string())
        } else if def.expr.is_empty() {
            Err("Write the condition every row must meet.".to_string())
        } else if dup {
            Err(format!("The table already has a check constraint called {}.", def.name))
        } else {
            self.structure().map_or_else(|| Err("The structure is not loaded.".into()), |st| self.alter_text(st, &all))
        };
        let warnings = self.structure().map(|st| ddl::warnings(st, &all)).unwrap_or_default();
        CheckView { def: f.def.clone(), changes_empty: changes.is_empty(), all, text, warnings }
    }

    pub(in crate::app) fn check_dialog(&mut self, ui: &mut egui::Ui, f: &mut CheckForm, keep: &mut bool) {
        let (dialect, version, _) = self.alter_env();
        let pg = dialect == Dialect::Postgres;
        let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        ui.set_width(560.0);
        dialog_head(ui, &Head::new(if f.from.is_some() { "Edit check" } else { "Add check" }).sub(format!("{table}.{}", f.from.as_ref().map_or("new check", |d| d.name.as_str()))).glyph(glyphs::list));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        form_grid(ui, "checkform", |ui| {
            form_label(ui, "Name");
            ui.add(egui::TextEdit::singleline(&mut f.def.name).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Check name"));
            ui.end_row();

            form_label(ui, "Condition");
            ui.add(egui::TextEdit::multiline(&mut f.def.expr).font(egui::TextStyle::Monospace).desired_rows(3).desired_width(f32::INFINITY).hint_text("total >= 0")).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Check condition"));
            ui.end_row();

            let can_not_enforce = dialect.can_not_enforce(&version);
            if pg || can_not_enforce {
                form_label(ui, "Options");
                ui.vertical(|ui| {
                    if pg {
                        ui.checkbox(&mut f.def.not_valid, "Do not check the rows already there (NOT VALID)").on_hover_text("The constraint holds for new and changed rows only, and the existing ones are left as they are. It skips the scan that holds an exclusive lock.");
                        ui.checkbox(&mut f.def.no_inherit, "Not inherited by child tables (NO INHERIT)");
                    }
                    if can_not_enforce {
                        ui.checkbox(&mut f.def.not_enforced, "Keep it without enforcing it (NOT ENFORCED)").on_hover_text("The server stores the constraint and never evaluates it, so rows that break it are accepted.");
                    }
                });
                ui.end_row();
            }
        });

        // The plan walks the whole structure, so it runs when the constraint changes and not on every frame.
        if f.view.as_ref().is_none_or(|v| v.def != f.def) {
            f.view = Some(self.check_view(f));
        }
        let v = f.view.as_ref().unwrap();
        let (text, changes_empty) = (&v.text, v.changes_empty);
        ui.add_space(6.0);
        caption(ui, "SQL TO RUN");
        ui.add_space(4.0);
        match text {
            Ok(t) => sql_block(ui, "check-sql", t, 130.0),
            Err(e) => {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(if changes_empty { FAINT() } else { ALERT() }));
            }
        }
        for w in &v.warnings {
            ui.label(RichText::new(w.as_str()).size(TXT_SMALL).color(WARNING()));
        }
        let text_ok = text.is_ok();
        let (mut stage, mut now) = (false, false);
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
            stage = ui.add_enabled(text_ok, primary_button(ui, "Stage change", PRIMARY())).clicked();
            let busy = self.applying();
            now = ui.add_enabled(text_ok && !self.sel_is_view() && !busy, egui::Button::new("Apply now")).on_hover_text("Stage this change and run everything staged. A locked connection asks to unlock first.").on_disabled_hover_text(if busy { BUSY_NOTE } else { "A view cannot be changed here" }).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if stage || now {
            *keep = false;
            let all = f.view.take().map(|v| v.all).unwrap_or_default();
            self.set_staged(restage(self.staged(), None, all, f.rest.len()));
            if now {
                self.request_alter_apply();
            }
        }
    }
}
