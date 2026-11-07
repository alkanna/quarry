//! The query bar: query inputs, history and the saved query library.

use super::*;

/// Recent Query, Sort and Project inputs of this collection; picking one fills that box.
/// Like the original's history menu (onHistory, renderer line 96157): each list starts with `{}`
/// to clear the box, and Sort also offers the key of every index of the collection.
pub(super) fn history_menu(ui: &mut egui::Ui, h: &QueryHist, index_sorts: &[String], acts: &mut Vec<QAct>) {
    sticky_menu(ui, "History".into(), |ui| {
        ui.set_min_width(360.0);
        ui.set_max_width(520.0);
        let mut sort = h.sort.clone();
        for k in index_sorts {
            if !sort.contains(k) {
                sort.push(k.clone());
            }
        }
        let with_clear = |l: &Vec<String>| std::iter::once("{}".to_string()).chain(l.iter().cloned()).collect::<Vec<_>>();
        let (sel, sort, proj) = (with_clear(&h.selector), with_clear(&sort), with_clear(&h.project));
        for (title, list, kind) in [("QUERY", &sel, 0), ("SORT  ·  history and indexes", &sort, 1), ("PROJECT", &proj, 2)] {
            caption(ui, title);
            egui::ScrollArea::vertical().id_salt(("hist", kind)).max_height(170.0).show(ui, |ui| {
                for v in list {
                    let one_line: String = v.split_whitespace().collect::<Vec<_>>().join(" ");
                    let mut job = json_job(&one_line.chars().take(90).collect::<String>());
                    job.wrap.max_rows = 1;
                    if ui.add(quiet_button(job)).on_hover_text(v.as_str()).clicked() {
                        acts.push(match kind {
                            0 => QAct::Filter(v.clone()),
                            1 => QAct::Sort(v.clone()),
                            _ => QAct::Project(v.clone()),
                        });
                        ui.close();
                    }
                }
            });
            ui.separator();
        }
    });
}

/// Named queries (or pipelines) saved for this collection: load, delete, or save the current one.
pub(super) fn library_menu(ui: &mut egui::Ui, lib: &[SavedQuery], new_name: &mut String, pipeline: bool, acts: &mut Vec<QAct>) {
    sticky_menu(ui, format!("Library ({})", lib.len()), |ui| {
        ui.set_min_width(320.0);
        if lib.is_empty() {
            ui.label(RichText::new(if pipeline { "No saved pipelines for this collection yet." } else { "No saved queries for this collection yet." }).color(MUTED()));
        }
        egui::ScrollArea::vertical().id_salt("lib").max_height(260.0).show(ui, |ui| {
            for q in lib {
                ui.horizontal(|ui| {
                    let preview = if pipeline { q.pipeline.clone().unwrap_or_default() } else { q.filter.clone() };
                    let preview: String = preview.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
                    if ui.add(quiet_button(RichText::new(&q.name).color(BRIGHT()))).on_hover_text(preview.as_str()).clicked() {
                        acts.push(QAct::Load(q.clone()));
                        ui.close();
                    }
                    ui.label(RichText::new(preview).monospace().size(TXT_CAPTION).color(FAINT()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🗙").on_hover_text("Delete").clicked() {
                            acts.push(QAct::Delete(q.name.clone()));
                        }
                    });
                });
            }
        });
        ui.separator();
        caption(ui, if pipeline { "SAVE THIS PIPELINE" } else { "SAVE THIS QUERY (query, sort and project)" });
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(new_name).hint_text("name").desired_width(200.0));
            let ok = !new_name.trim().is_empty();
            if ui.add_enabled(ok, egui::Button::new("Save")).clicked() || (ok && r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                acts.push(QAct::Save(new_name.trim().to_string()));
                new_name.clear();
            }
        });
        ui.label(RichText::new("Saving under an existing name replaces it.").size(TXT_CAPTION).color(FAINT()));
    });
}

impl App {
    /// A query input that grows with its content, with highlighting and autocomplete.
    /// Enter runs it (or picks a suggestion), Shift+Enter adds a line.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn query_field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str, width: f32, kind: FieldKind, paths: &[String], dates: &[String], ac: &mut Ac) -> bool {
        ui.add_sized([56.0, 20.0], egui::Label::new(caption_text(label)));
        let id = egui::Id::unique(("query", label));
        // Test switch: show the suggestion list in the demo picture.
        if ac.id.is_none() && kind == FieldKind::Query && std::env::var("QUARRY_TEST_AC").is_ok() {
            // QUARRY_TEST_AC=date shows the date list after a date field's colon instead.
            let date = std::env::var("QUARRY_TEST_AC").is_ok_and(|v| v == "date");
            let before = if date { "{ createdAt: " } else { "{ status: { $i" };
            if let Some((token, items)) = suggest(before, kind, paths, dates) {
                *ac = Ac { id: Some(id), token, items, sel: 0, cursor: before.chars().count(), rect: None };
            }
        }
        let (out, picked) = ac_field(ui, id, text, ac, &|b| suggest(b, kind, paths, dates), |ui, text, open| {
            egui::TextEdit::multiline(text)
                .id(id)
                .font(egui::TextStyle::Monospace)
                .hint_text(hint)
                .desired_width(width)
                .desired_rows(1)
                .lock_focus(open)
                .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, Key::Enter))
                .layouter(&mut code_layout)
                .show(ui)
        });
        !picked && out.response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift)
    }

    pub(super) fn save_to_library(&mut self, q: SavedQuery) {
        let key = self.col_key(false);
        let list = self.prefs.library.entry(key).or_default();
        let name = q.name.clone();
        list.retain(|x| !(x.name == q.name && x.pipeline.is_some() == q.pipeline.is_some()));
        list.push(q);
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.save_prefs();
        self.ok(format!("Saved \"{name}\" to the library"));
    }

    pub(super) fn delete_from_library(&mut self, name: &str, pipeline: bool) {
        let key = self.col_key(false);
        if let Some(list) = self.prefs.library.get_mut(&key) {
            list.retain(|x| !(x.name == name && x.pipeline.is_some() == pipeline));
        }
        self.save_prefs();
    }
}
