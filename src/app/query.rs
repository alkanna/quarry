//! The query bar: query inputs and history. The script library's quick picker is in library/picker.rs.

use super::*;

/// Recent Query, Sort and Project inputs of this collection; picking one fills that box.
/// Like the original's history menu (onHistory, renderer line 96157): each list can clear its box,
/// and Sort also offers the key of every index of the collection.
pub(super) fn history_menu(ui: &mut egui::Ui, h: &QueryHist, index_sorts: &[String], acts: &mut Vec<QAct>) {
    icon_menu(ui, paint_clock, true, |ui| {
        popover_body(ui, 380.0, |ui| {
            popover_head(ui, "History", "", "Pick an entry to fill its box.");
            // The open list and the filter outlive the menu, so it reopens where it was left.
            let (tab_id, filter_id) = (ui.make_persistent_id("hist-tab"), ui.make_persistent_id("hist-filter"));
            let mut tab: usize = ui.data(|d| d.get_temp(tab_id)).unwrap_or(0);
            let mut needle: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
            let from_index: Vec<&String> = index_sorts.iter().filter(|k| !h.sort.contains(k)).collect();
            let labels = [format!("Query  {}", h.selector.len()), format!("Sort  {}", h.sort.len() + from_index.len()), format!("Project  {}", h.project.len())];
            segmented(ui, &mut tab, &[(0, labels[0].as_str()), (1, labels[1].as_str()), (2, labels[2].as_str())]);
            ui.add_space(8.0);
            well_field(ui, filter_id, FieldIcon::Search, None, |ui| ui.add(field_edit(&mut needle, filter_id, "Filter history", ui.available_width(), egui::TextStyle::Body)));
            ui.data_mut(|d| {
                d.insert_temp(tab_id, tab);
                d.insert_temp(filter_id, needle.clone());
            });
            ui.add_space(8.0);
            // Entries, each with whether it comes from an index rather than from history.
            let entries: Vec<(&String, bool)> = match tab {
                0 => h.selector.iter().map(|v| (v, false)).collect(),
                1 => h.sort.iter().map(|v| (v, false)).chain(from_index.iter().map(|v| (*v, true))).collect(),
                _ => h.project.iter().map(|v| (v, false)).collect(),
            };
            let needle = needle.trim().to_lowercase();
            let entries: Vec<_> = entries.into_iter().filter(|(v, _)| needle.is_empty() || v.to_lowercase().contains(&needle)).collect();
            let act = |v: String| match tab {
                0 => QAct::Filter(v),
                1 => QAct::Sort(v),
                _ => QAct::Project(v),
            };
            let (row, _) = pop_row(ui, POP_ROW_H);
            let p = ui.painter();
            mono_at(p, row.rect.left(), row.rect.center().y, "{}", FAINT());
            let clear = format!("Clear the {} box", ["Query", "Sort", "Project"][tab]);
            galley_at(p, row.rect.left() + 26.0, row.rect.center().y, p.layout_no_wrap(clear, FontId::proportional(TXT_BODY), MUTED()), MUTED());
            if row.clicked() {
                acts.push(act("{}".into()));
                ui.close();
            }
            ui.add_space(4.0);
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, RAISED_LINE()));
            ui.add_space(4.0);
            if entries.is_empty() {
                ui.add_space(4.0);
                if needle.is_empty() {
                    empty_state(ui, paint_clock, "No history yet", "What you run here shows up in this list.", |_| {});
                } else {
                    empty_state(ui, paint_search, "Nothing matches", "", |_| {});
                }
            }
            egui::ScrollArea::vertical().id_salt("hist").max_height(300.0).show(ui, |ui| {
                for (v, from_index) in entries {
                    let (row, _) = pop_row(ui, POP_ROW_H);
                    let rect = row.rect;
                    let p = ui.painter();
                    let right = if from_index { tag_at(p, rect.right(), rect.center().y, "index", MUTED()) - 8.0 } else { rect.right() };
                    let mut job = json_job(&one_line(v));
                    job.wrap = egui::text::TextWrapping::truncate_at_width(right - rect.left());
                    galley_at(p, rect.left(), rect.center().y, p.layout_job(job), TEXT());
                    if row.on_hover_text(v.as_str()).clicked() {
                        acts.push(act(v.clone()));
                        ui.close();
                    }
                }
            });
            popover_foot(ui, None, &["Kept for each collection.", "Sort also lists the key of every index."]);
        });
    })
    .on_hover_text("History");
}

/// The start of `s` on one line, for a preview. Only the start shows, so only it gets colored.
fn one_line(s: &str) -> String {
    s.chars().take(200).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

impl App {
    /// In the offline demo with QUARRY_TEST_SQL=library, holds the library menu open for the picture.
    pub(super) fn demo_open_library(&self, ctx: &egui::Context, button: &egui::Response) {
        if self.demo && std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("library") {
            egui::Popup::open_id(ctx, egui::Popup::default_response_id(button));
        }
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
        let compute = |b: &str, _: bool| {
            if kind == FieldKind::Query {
                edited.set(Some(value_at(b).and_then(|(_, p)| p).map(str::to_string)));
            }
            suggest(b, kind, paths, fields)
        };
        // In Normal mode Vim takes Enter as a motion, so it is read first to keep Enter running the query.
        let normal_enter = crate::ui::vim::normal(ui.ctx(), id) && ui.memory(|m| m.has_focus(id)) && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);
        let (out, picked) = crate::ui::vim::wrap_with(ui, id, text, |ui, text| {
            ac_field(ui, id, text, ac, &compute, |ui, text, open| {
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
            })
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
        !picked && out.response.has_focus() && (normal_enter || ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift))
    }
}
