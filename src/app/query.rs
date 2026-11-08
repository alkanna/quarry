//! The query bar: query inputs, history and the saved query library.

use super::*;

/// Recent Query, Sort and Project inputs of this collection; picking one fills that box.
/// Like the original's history menu (onHistory, renderer line 96157): each list starts with `{}`
/// to clear the box, and Sort also offers the key of every index of the collection.
pub(super) fn history_menu(ui: &mut egui::Ui, h: &QueryHist, index_sorts: &[String], acts: &mut Vec<QAct>) {
    icon_menu(ui, paint_clock, true, |ui| {
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
    })
    .on_hover_text("History");
}

/// Named queries (or pipelines) saved for this collection: load, delete, or save the current one.
/// `icon` shows it as a book icon, as in the find bar.
pub(super) fn library_menu(ui: &mut egui::Ui, lib: &[SavedQuery], new_name: &mut String, pipeline: bool, icon: bool, acts: &mut Vec<QAct>) {
    let title = format!("Library ({})", lib.len());
    let body = |ui: &mut egui::Ui| {
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
    };
    if icon {
        icon_menu(ui, paint_book, true, body).on_hover_text(title);
    } else {
        sticky_menu(ui, title, body);
    }
}

/// The id of a query box. Shortcuts focus it by this, and the find bar reads its focus and hover.
pub(super) fn query_id(label: &str) -> egui::Id {
    egui::Id::unique(("query", label))
}

/// Inner margin of the find bar's editors. Mono ink sits high in its line, so the text gets all
/// the vertical room above it to center on the buttons and labels beside it.
pub(super) fn editor_margin() -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin { left: 2, right: 2, top: 4, bottom: 0 })
}

/// Where the server lookup of a field's values stands.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Lookup {
    Pending,
    Done,
    /// Typed text is not looked up, for this reason.
    Skipped(&'static str),
}

/// Where the query box's caret is, for the server lookups of field values.
#[derive(Default)]
pub(super) struct Typed {
    /// The field whose value the caret is in.
    pub(super) field: Option<String>,
    /// Set when the box was edited this frame: the text typed so far in a string value, if any.
    pub(super) edit: Option<Option<String>>,
}

impl App {
    /// A query input that grows with its content, with highlighting and autocomplete. It has no
    /// frame or label: the find bar and option chips draw those around it.
    /// Enter runs it (or picks a suggestion), Shift+Enter adds a line.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn query_field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str, width: f32, kind: FieldKind, paths: &[String], fields: &Fields, ac: &mut Ac, typed: &mut Typed, lookup: &dyn Fn(&str) -> Lookup) -> bool {
        let id = query_id(label);
        let frame = editor_margin();
        // Test switch: show the suggestion list in the demo picture.
        if ac.id.is_none() && kind == FieldKind::Query && std::env::var("QUARRY_TEST_AC").is_ok() {
            // QUARRY_TEST_AC=date shows the date list after a date field's colon instead.
            let date = std::env::var("QUARRY_TEST_AC").is_ok_and(|v| v == "date");
            let before = if date { "{ createdAt: " } else { "{ status: { $i" };
            if let Some((token, items)) = suggest(before, kind, paths, fields) {
                *ac = Ac { id: Some(id), token, items, cursor: before.chars().count(), ..Default::default() };
            }
        }
        // Suggestions are recomputed on every edit, even with no list open, so this sees every edit.
        let edited = std::cell::Cell::new(None);
        let compute = |b: &str| {
            if kind == FieldKind::Query {
                edited.set(Some(value_at(b).and_then(|(_, p)| p).map(str::to_string)));
            }
            suggest(b, kind, paths, fields)
        };
        let (out, picked) = ac_field(ui, id, text, ac, &compute, |ui, text, open| {
            egui::TextEdit::multiline(text)
                .id(id)
                .frame(frame)
                .font(egui::TextStyle::Monospace)
                .hint_text(RichText::new(hint).monospace().color(FAINT()))
                .desired_width(width)
                .desired_rows(1)
                .lock_focus(open)
                .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, Key::Enter))
                .layouter(&mut code_layout)
                .show(ui)
        });
        typed.edit = edited.take();
        let here = out.cursor_range.filter(|r| r.is_empty() && kind == FieldKind::Query && out.response.has_focus()).and_then(|r| {
            let ci = r.primary.index.0;
            let before: String = text.chars().take(ci).collect();
            value_at(&before).map(|(f, _)| (f.to_string(), ci))
        });
        // While the field's values are fetched the list stays open, empty or with the page's values,
        // and says so; when they arrive it is rebuilt without waiting for another key. A skipped
        // typed lookup is noted under the list, opened for it only by typing so Esc still closes it.
        let state = here.as_ref().map_or(Lookup::Done, |(p, _)| lookup(p));
        let note = match state {
            Lookup::Skipped(n) => Some(n),
            _ => None,
        };
        let was_loading = ac.id == Some(id) && ac.loading;
        if let Some((_, ci)) = &here
            && (state == Lookup::Pending || note.is_some() && (ac.shown(id) || typed.edit.is_some()))
        {
            if ac.id != Some(id) || ac.items.is_empty() {
                *ac = Ac { id: Some(id), cursor: *ci, ..Default::default() };
            }
            (ac.loading, ac.note) = (state == Lookup::Pending, note);
        }
        if was_loading && state != Lookup::Pending {
            ac.loading = false;
            let before: String = text.chars().take(ac.cursor).collect();
            if let (true, Some((token, items))) = (here.is_some(), suggest(&before, kind, paths, fields)) {
                (ac.token, ac.items, ac.sel) = (token, items, 0);
            }
        }
        typed.field = here.map(|h| h.0);
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
