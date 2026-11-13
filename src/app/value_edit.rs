//! The large value editor for Mongo fields and documents and for SQL cells. It opens as a dialog or, with the Editing setting,
//! as a panel docked beside the page (docs/long-values.figcraft, options A and C).

use super::*;

#[derive(PartialEq, Clone, Copy)]
pub(super) enum ValueAct {
    None,
    Apply,
    Close,
    Null,
}

/// A value the one-line editors cannot show faithfully, so double-click opens the large editor straight away.
pub(super) fn is_long(s: &str) -> bool {
    s.contains('\n') || s.len() > 500
}

/// The editor, its error and the footer, shared by the dialog and the panel. `null` labels a Set null button. `rows` is the
/// panel's height in lines; None is the dialog, whose height follows the text. `dirty` is set by an edit.
#[allow(clippy::too_many_arguments)]
fn value_body(ui: &mut egui::Ui, id: egui::Id, text: &mut String, code: bool, focus: &mut bool, dirty: &mut bool, err: Option<&str>, null: Option<&str>, rows: Option<usize>, max_h: f32) -> ValueAct {
    let modal = rows.is_none();
    let wrap_id = egui::Id::unique("value-editor-wrap");
    let mut wrap = ui.data(|d| d.get_temp(wrap_id)).unwrap_or(true);
    // Counted again only after an edit: the editor is meant for values of megabytes, too long to walk every frame.
    let count_id = id.with("counts");
    let (chars, lines) = match ui.data(|d| d.get_temp::<(usize, usize, usize)>(count_id)) {
        Some((len, c, l)) if len == text.len() => (c, l),
        _ => {
            let (c, l) = (text.chars().count(), text.lines().count().max(1));
            ui.data_mut(|d| d.insert_temp(count_id, (text.len(), c, l)));
            (c, l)
        }
    };
    let rows = rows.unwrap_or(lines.clamp(12, 40));
    // JSON text, typed into any column or field, can be read as a folding tree; edits are made in the text.
    let json = crate::ui::json_tree::looks_like(text);
    let tree = crate::ui::json_tree::is_tree(ui, id, text);
    if tree {
        crate::ui::json_tree::scrolled(ui, id, text, rows as f32 * ui.text_style_height(&egui::TextStyle::Monospace), max_h);
    } else {
        let find = Find::bar(ui, id, text);
        egui::ScrollArea::both().id_salt(id.with("scroll")).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            let mut lay = find.layouter(code);
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, w: f32| lay(ui, buf, if wrap { w } else { f32::INFINITY });
            // The default layouter caches its galley; the custom one rebuilds the whole text every frame, so it is used only when needed.
            let custom = code || find.open || !wrap;
            let mut edit = |ui: &mut egui::Ui, text: &mut String| {
                let te = egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(rows);
                if custom { te.layouter(&mut layouter).show(ui) } else { te.show(ui) }
            };
            // Pairing quotes and brackets helps JSON but gets in the way of plain text.
            let out = if code { with_autopair(ui, id, text, edit) } else { edit(ui, text) };
            find.reveal(ui, text, &out);
            if out.response.changed() {
                *dirty = true;
                ui.data_mut(|d| d.remove::<(usize, usize, usize)>(count_id));
            }
            if std::mem::take(focus) {
                out.response.request_focus();
            }
        });
    }
    if let Some(e) = err {
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(e).size(TXT_SMALL).color(ALERT())).wrap());
    }
    let mut act = ValueAct::None;
    // The panel shares the window with the page, so its keys count only while its text has focus.
    let focused = ui.memory(|m| m.has_focus(id));
    if (modal || focused) && ui.input(|i| i.modifiers.command && (i.key_pressed(Key::Enter) || (modal && i.key_pressed(Key::S)))) {
        act = ValueAct::Apply;
    }
    if !modal && focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        act = ValueAct::Close;
    }
    ui.add_space(8.0);
    // Wrapped so right_group can drop the buttons to their own row when the side panel is narrow.
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("{chars} characters · {lines} {}", if lines == 1 { "line" } else { "lines" })).size(TXT_SMALL).color(FAINT()));
        ui.add_space(4.0);
        if !tree && ui.checkbox(&mut wrap, "Wrap lines").changed() {
            ui.data_mut(|d| d.insert_temp(wrap_id, wrap));
        }
        if json {
            ui.add_space(4.0);
            crate::ui::json_tree::mode_switch(ui, id);
        }
        right_group(ui, |ui| {
            if primary(ui, "Apply", PRIMARY()).on_hover_text("Cmd+Enter").clicked() {
                act = ValueAct::Apply;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                act = ValueAct::Close;
            }
            if let Some(l) = null && quiet(ui, l).clicked() {
                act = ValueAct::Null;
            }
        });
    });
    act
}

impl App {
    /// The large editor shows in the docked panel rather than a dialog. A whole document always gets the dialog.
    pub(super) fn value_in_panel(&self) -> bool {
        self.prefs.value_panel && (self.json_edit.as_ref().is_some_and(|je| !je.path.is_empty()) || self.sql_cell.as_ref().is_some_and(|e| e.big))
    }

    /// The large editor shows as a dialog, which takes the keys from the page.
    pub(super) fn value_dialog_open(&self) -> bool {
        (self.json_edit.is_some() || self.sql_cell.as_ref().is_some_and(|e| e.big)) && !self.value_in_panel()
    }

    /// The side panel holds text not applied yet, which opening another value would throw away.
    pub(super) fn panel_dirty(&self) -> bool {
        self.value_in_panel() && (self.json_edit.as_ref().is_some_and(|j| j.dirty) || self.sql_cell.as_ref().is_some_and(|e| e.big && e.dirty))
    }

    /// The large editor as a dialog, from `modals`.
    pub(super) fn value_dialog(&mut self, ctx: &egui::Context) {
        if !self.value_dialog_open() {
            return;
        }
        // Esc or a click outside closes it, unless a button already decided.
        let close = |r: &egui::ModalResponse<()>, act: ValueAct| if r.should_close() && act == ValueAct::None { ValueAct::Close } else { act };
        let screen = ctx.content_rect();
        let max_h = screen.height() * 0.62;
        let width = (screen.width() * 0.72).clamp(480.0, 1200.0);
        if let Some(mut je) = self.json_edit.take() {
            let mut act = ValueAct::None;
            let r = dialog(egui::Id::unique(("quarry-layer", "jsonedit"))).show(ctx, |ui| {
                ui.set_width(width);
                act = self.json_edit_body(ui, &mut je, None, max_h);
            });
            self.json_edit_act(je, close(&r, act));
        }
        if let Some(mut e) = self.sql_cell.take_if(|e| e.big) {
            let mut act = ValueAct::None;
            let r = dialog(egui::Id::unique(("quarry-layer", "sqlcell"))).show(ctx, |ui| {
                ui.set_width(width);
                act = self.sql_cell_body(ui, &mut e, None, max_h);
            });
            sql_cell_act(&mut e, close(&r, act));
            self.sql_cell = Some(e);
        }
    }

    /// The large editor docked on the right of the page. Double-clicking another value while it is open loads that one.
    pub(super) fn value_panel(&mut self, ui: &mut egui::Ui) {
        if !self.value_in_panel() {
            return;
        }
        egui::Panel::right("value-panel").default_size(380.0).size_range(280.0..=900.0).resizable(true).frame(egui::Frame::new().fill(PANEL()).inner_margin(12)).show(ui, |ui| {
            let max_h = (ui.available_height() - 120.0).max(80.0);
            let rows = (max_h / ui.text_style_height(&egui::TextStyle::Monospace)) as usize;
            if let Some(mut je) = self.json_edit.take() {
                let act = self.json_edit_body(ui, &mut je, Some(rows), max_h);
                self.json_edit_act(je, act);
            } else if let Some(mut e) = self.sql_cell.take() {
                let act = self.sql_cell_body(ui, &mut e, Some(rows), max_h);
                sql_cell_act(&mut e, act);
                self.sql_cell = Some(e);
            }
        });
    }

    fn json_edit_body(&mut self, ui: &mut egui::Ui, je: &mut JsonEdit, rows: Option<usize>, max_h: f32) -> ValueAct {
        ui.horizontal(|ui| {
            let title = if je.path.is_empty() { "Edit document".to_string() } else { format!("Edit {}", je.path.join(".")) };
            dialog_title(ui, title);
            if !je.path.is_empty() {
                ui.add_space(8.0);
                Self::type_combo(ui, "jsonkind", &mut je.kind);
            }
            if let Some(id) = je.base.get("_id").filter(|_| rows.is_none()) {
                right_group(ui, |ui| {
                    ui.label(RichText::new(format!("_id {}", db::short(id))).font(FontId::monospace(TXT_SMALL)).color(FAINT()));
                });
            }
        });
        let hint = if je.kind == "string" { "Type null for a null value. Applying keeps it as an unsaved edit until you press Save." } else { "JSON, JSON5 or shell syntax. Applying keeps it as an unsaved edit until you press Save." };
        ui.label(RichText::new(hint).size(TXT_SMALL).color(MUTED()));
        ui.add_space(4.0);
        let null = (!je.path.is_empty()).then_some("Set null");
        value_body(ui, egui::Id::unique("json-editor-text"), &mut je.text, je.kind != "string", &mut je.focus, &mut je.dirty, None, null, rows, max_h)
    }

    fn json_edit_act(&mut self, je: JsonEdit, act: ValueAct) {
        let new = match act {
            ValueAct::None => return self.json_edit = Some(je),
            ValueAct::Close => return,
            ValueAct::Null => db::edit_path(&je.base, &je.path, Some(Bson::Null)),
            ValueAct::Apply if je.path.is_empty() => db::parse_doc(&je.text).and_then(|mut new| {
                match (je.base.get("_id"), new.get("_id")) {
                    (Some(a), Some(b)) if a != b => return Err("The _id of a document cannot change. Use Duplicate to make a copy.".to_string()),
                    (Some(a), None) => {
                        // Keep _id first, as MongoDB stores it.
                        let mut d = doc! { "_id": a.clone() };
                        d.extend(new);
                        new = d;
                    }
                    _ => {}
                }
                Ok(new)
            }),
            ValueAct::Apply => db::make_value(je.kind, &je.text).and_then(|v| db::edit_path(&je.base, &je.path, Some(v))),
        };
        match new {
            Ok(n) => {
                let base = je.base.clone();
                self.stage(&base, Ok(n));
            }
            Err(e) => {
                self.err(e);
                self.json_edit = Some(je);
            }
        }
    }

    fn sql_cell_body(&mut self, ui: &mut egui::Ui, e: &mut CellEdit, rows: Option<usize>, max_h: f32) -> ValueAct {
        let col = self.sql_col(&e.col).map(|c| (c.ty.to_uppercase(), c.nullable));
        let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            dialog_title(ui, format!("Edit {}", e.col));
            if let Some((ty, _)) = &col {
                ui.add_space(8.0);
                badge(ui, ty, MUTED());
            }
            if rows.is_none() {
                let id = e.id.as_ref().map(db::short).unwrap_or_default();
                right_group(ui, |ui| {
                    ui.label(RichText::new(format!("{table} · {id}")).font(FontId::monospace(TXT_SMALL)).color(FAINT()));
                });
            }
        });
        ui.label(RichText::new("Type NULL for a null value. Applying keeps it as an unsaved edit until you press Save.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(4.0);
        let null = col.as_ref().is_none_or(|c| c.1).then_some("Set NULL");
        // JSON and jsonb columns get the code look and are checked as they are typed, with the parse Apply runs, which
        // also refuses to apply text that does not parse.
        let json = self.sql_col(&e.col).is_some_and(|c| c.kind == db::sql::value::Kind::Json);
        // ponytail: past 1 MB each keystroke would parse the whole text, so such values are checked on Apply only
        if json && e.json_err.is_none() && e.text.len() <= 1 << 20 {
            e.json_err = Some(self.sql_col(&e.col).and_then(|c| db::sql::value::parse_col(self.sql_dialect(), c, &e.text).err()));
        }
        // Once checked, the live result replaces the error a failed Apply left for older text.
        let err = match &e.json_err { Some(r) => r.as_deref(), None => e.err.as_deref() };
        let mut edited = false;
        let act = value_body(ui, egui::Id::unique("sql-cell-text"), &mut e.text, json, &mut e.focus, &mut edited, err, null, rows, max_h);
        if edited {
            e.dirty = true;
            e.json_err = None;
        }
        act
    }
}

/// `sql_cell_finish` stages the result after the grid draws, or keeps the editor open with its error.
fn sql_cell_act(e: &mut CellEdit, act: ValueAct) {
    match act {
        ValueAct::None => {}
        ValueAct::Apply => e.done = Some(true),
        ValueAct::Close => e.done = Some(false),
        ValueAct::Null => {
            e.text = "NULL".into();
            e.done = Some(true);
        }
    }
}
