//! The documents view: query bar, results table and list, columns and row selection.

use super::*;

/// Recent Query, Sort and Project inputs of this collection; picking one fills that box.
/// Like the original's history menu (onHistory, renderer line 96157): each list starts with `{}`
/// to clear the box, and Sort also offers the key of every index of the collection.
pub(crate) fn history_menu(ui: &mut egui::Ui, h: &QueryHist, index_sorts: &[String], acts: &mut Vec<QAct>) {
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
pub(crate) fn library_menu(ui: &mut egui::Ui, lib: &[SavedQuery], new_name: &mut String, pipeline: bool, acts: &mut Vec<QAct>) {
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

/// The original's "optimal" column width (measureColumnWidth, renderer line 31273): the widest
/// cell, kept between 40 and 400 px, plus 10 px. Our cells add 16 px of inner padding on top.
pub(crate) fn optimal_width(ui: &egui::Ui, docs: &[Document], col: &str, summarize: bool) -> f32 {
    if docs.is_empty() {
        return 100.0 + CELL_PAD; // the original's fallback width
    }
    let widest = ui.fonts_mut(|f| {
        let head = f.layout_no_wrap(col.to_string(), FontId::proportional(TXT_BODY), TEXT()).size().x;
        docs.iter()
            .filter_map(|d| db::get_path(d, col))
            .map(|v| f.layout_job(column_job(v, summarize)).size().x)
            .fold(head, f32::max)
    });
    widest.clamp(OPTIMAL_MIN_W, OPTIMAL_MAX_W) + OPTIMAL_PAD + CELL_PAD
}

pub(crate) fn visible_cols(p: Option<&ColPrefs>, fields: &[String], n: usize) -> Vec<String> {
    let base: Vec<String> = match p.and_then(|p| p.shown.clone()) {
        Some(v) => v,
        None => fields.iter().take(n).cloned().collect(),
    };
    let pinned: Vec<String> = p.map(|p| p.pinned.clone()).unwrap_or_default();
    let mut out: Vec<String> = pinned.iter().filter(|c| base.contains(c)).cloned().collect();
    out.extend(base.into_iter().filter(|c| !pinned.contains(c)));
    out
}

#[derive(Clone, Copy)]
pub(crate) enum DocsAct {
    CopyJs,
    CopyJson,
    CopyLines,
    Duplicate,
    Export,
    Filter,
    Update,
    Delete,
}

/// Positions in `new` of the documents at `rows` in `old`, matched by _id.
pub(crate) fn follow_ids(old: &[Document], new: &[Document], rows: impl Iterator<Item = usize>) -> BTreeSet<usize> {
    let ids: HashSet<String> = rows.filter_map(|i| old.get(i)?.get("_id").map(db::compact)).collect();
    if ids.is_empty() {
        return BTreeSet::new();
    }
    new.iter().enumerate().filter(|(_, d)| d.get("_id").is_some_and(|v| ids.contains(&db::compact(v)))).map(|(i, _)| i).collect()
}

/// Actions on one document or the checked ones, shared by the selection bar and the row menus.
pub(crate) fn docs_menu(ui: &mut egui::Ui, n: usize, heading: bool) -> Option<DocsAct> {
    if heading {
        ui.label(RichText::new(if n == 1 { "Document".to_string() } else { format!("{n} selected documents") }).size(TXT_CAPTION).color(FAINT()));
    }
    let items: [(&str, DocsAct); 8] = [
        ("Copy as JS", DocsAct::CopyJs),
        ("Copy as JSON", DocsAct::CopyJson),
        ("Copy as JSON lines", DocsAct::CopyLines),
        ("Duplicate", DocsAct::Duplicate),
        ("Export…", DocsAct::Export),
        ("Filter to these", DocsAct::Filter),
        ("Update in shell", DocsAct::Update),
        ("Delete…", DocsAct::Delete),
    ];
    let mut act = None;
    for (label, a) in items {
        let text = if matches!(a, DocsAct::Delete) { RichText::new(label).color(ALERT()) } else { RichText::new(label) };
        if ui.button(text).clicked() {
            act = Some(a);
        }
    }
    act
}

/// The row menu: acts on every checked document when the row is one of them, else on that row.
pub(crate) fn row_menu(ui: &mut egui::Ui, i: usize, checked: &BTreeSet<usize>) -> Option<(usize, DocsAct)> {
    docs_menu(ui, if checked.contains(&i) { checked.len() } else { 1 }, true).map(|a| (i, a))
}
impl App {
    pub(crate) fn collection(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        let (coll, view) = (t.sel.coll.clone(), t.view);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&coll).size(TXT_HEADING).strong().color(BRIGHT()));
            ui.add_space(12.0);
            for (v, label) in [(View::Documents, "Documents"), (View::Aggregate, "Aggregate"), (View::Indexes, "Indexes"), (View::Schema, "Schema"), (View::Import, "Import"), (View::Compare, "Compare"), (View::Shell, "Shell")] {
                if ui.selectable_label(view == v, label).clicked() {
                    self.set_view(v);
                }
            }
            if matches!(view, View::Documents | View::Aggregate) {
                right_group(ui, |ui| {
                    if self.prefs.table {
                        self.columns_menu(ui, view == View::Aggregate);
                    }
                    let mut table = self.prefs.table;
                    ui.selectable_value(&mut table, false, "JSON");
                    ui.selectable_value(&mut table, true, "Table");
                    if table != self.prefs.table {
                        self.prefs.table = table;
                        self.save_prefs();
                    }
                });
            }
        });
        ui.add_space(6.0);
        match view {
            View::Documents => self.documents(ui),
            View::Aggregate => self.aggregate(ui),
            View::Indexes => self.indexes_view(ui),
            View::Shell => self.shell_view(ui),
            View::Schema => self.schema_view(ui),
            View::Import => self.import_view(ui),
            View::Compare => self.compare_view(ui),
        }
    }

    /// A query input that grows with its content, with highlighting and autocomplete.
    /// Enter runs it (or picks a suggestion), Shift+Enter adds a line.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn query_field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str, width: f32, kind: FieldKind, paths: &[String], dates: &[String], ac: &mut Ac) -> bool {
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

    pub(crate) fn documents(&mut self, ui: &mut egui::Ui) {
        let mut run = false;
        let mut explain = false;
        let mut code_lang = None;
        let key = self.col_key(false);
        let hist = self.prefs.history.get(&key).cloned().unwrap_or_default();
        let index_sorts: Vec<String> = self.t().map(|t| t.indexes.iter().filter_map(|ix| ix.get_document("keys").ok()).map(|k| db::shell_text(&Bson::Document(k.clone()))).collect()).unwrap_or_default();
        let lib: Vec<SavedQuery> = self.prefs.library.get(&key).map(|v| v.iter().filter(|q| q.pipeline.is_none()).cloned().collect()).unwrap_or_default();
        let mut qacts = Vec::new();
        let (t, ac, new_lib) = (&mut self.tabs[self.cur], &mut self.ac, &mut self.new_lib);
        let paths = std::mem::take(&mut t.paths);
        let dates = t.date_paths.clone();
        toolbar(ui, |ui| {
            ui.horizontal_top(|ui| {
                let w = ui.available_width() - 490.0;
                run |= Self::query_field(ui, "QUERY", &mut t.filter_txt, "{ field: value }   or paste db.coll.find(...) / db.coll.aggregate([...])", w, FieldKind::Query, &paths, &dates, ac);
                if primary(ui, "  Find  ", PRIMARY()).on_hover_text("Enter").clicked() {
                    run = true;
                }
                explain |= ui.button("Explain").on_hover_text("Cmd+E").clicked();
                code_menu(ui, &mut code_lang);
                history_menu(ui, &hist, &index_sorts, &mut qacts);
                library_menu(ui, &lib, new_lib, false, &mut qacts);
            });
            // A pasted shell query carries its own sort and projection.
            if !db::is_shell(&t.filter_txt) {
                ui.horizontal_top(|ui| {
                    let w = (ui.available_width() - 160.0) / 2.0;
                    run |= Self::query_field(ui, "SORT", &mut t.sort_txt, "{ _id: -1 }", w, FieldKind::Sort, &paths, &[], ac);
                    run |= Self::query_field(ui, "PROJECT", &mut t.proj_txt, "{ name: 1 }", w, FieldKind::Project, &paths, &[], ac);
                });
            }
        });
        t.paths = paths;
        for a in qacts {
            let t = &mut self.tabs[self.cur];
            match a {
                QAct::Filter(v) => t.filter_txt = v,
                QAct::Sort(v) => t.sort_txt = v,
                QAct::Project(v) => t.proj_txt = v,
                QAct::Load(q) => {
                    (t.filter_txt, t.sort_txt, t.proj_txt) = (q.filter, q.sort, q.project);
                    run = true;
                }
                QAct::Save(name) => {
                    let q = SavedQuery { name: name.clone(), filter: t.filter_txt.clone(), sort: t.sort_txt.clone(), project: t.proj_txt.clone(), pipeline: None };
                    self.save_to_library(q);
                }
                QAct::Delete(name) => self.delete_from_library(&name, false),
            }
        }
        if run {
            self.run_query();
        }
        if explain {
            self.explain();
        }
        if let Some(l) = code_lang {
            self.show_code(l, true);
        }
        ui.add_space(6.0);
        let t = self.t().unwrap();
        let (count, ms, loading, page) = (t.count, t.ms, t.loading, t.page);
        let pages = count.div_ceil(PAGE_SIZE).max(1);
        let mut go: Option<u64> = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} documents", group(&count.to_string()))).color(BRIGHT()));
            ui.label(RichText::new(format!("{ms} ms")).color(MUTED()));
            if loading {
                spin(ui);
            }
            ui.separator();
            if ui.add_enabled(page > 0, egui::Button::new("‹")).clicked() {
                go = Some(page - 1);
            }
            ui.label(format!("page {} of {}", page + 1, pages));
            if ui.add_enabled(page + 1 < pages, egui::Button::new("›")).clicked() {
                go = Some(page + 1);
            }
            if ui.button("⟳ Reload").clicked() {
                go = Some(page);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("More", |ui| {
                    if ui.button("Open query in Shell").clicked() {
                        let t = &mut self.tabs[self.cur];
                        t.shell.code = query_script(&t.sel.coll, &t.filter_txt);
                        t.view = View::Shell;
                        t.selected = None;
                        ui.close();
                    }
                    if ui.button("Update matching…").clicked() {
                        self.modal = Some(Modal::UpdateMany { set_txt: "{\n  \n}".into(), unset_txt: String::new(), preview: None });
                        ui.close();
                    }
                    ui.separator();
                    let t = self.t().unwrap();
                    let (coll, filter) = (t.sel.coll.clone(), t.query.filter.clone());
                    if ui.button("Delete all matching documents…").clicked() {
                        self.modal = Some(Modal::Typed {
                            title: format!("Delete every document in {coll} matching the current filter (about {count} documents)?"),
                            expect: coll.clone(),
                            typed: String::new(),
                            action: Danger::DeleteMatching(filter),
                        });
                        ui.close();
                    }
                    if ui.button("Drop collection…").clicked() {
                        self.modal = Some(Modal::Typed {
                            title: format!("Drop the collection {coll} with all its documents and indexes? This cannot be undone."),
                            expect: coll,
                            typed: String::new(),
                            action: Danger::DropCollection,
                        });
                        ui.close();
                    }
                });
                if ui.button("📤 Export…").clicked() {
                    self.export();
                }
                if ui.button("✚ Insert").clicked() {
                    self.insert = Some("{\n  \n}".into());
                }
                let pending = self.tab_drafts();
                if !pending.is_empty() && primary(ui, &format!("Save all ({})", pending.len()), WARNING()).on_hover_text("Documents with unsaved edits").clicked() {
                    if !self.sel_writable() {
                        self.ask_unlock();
                    } else if self.prefs.confirm_save && !self.prefs.always_writable {
                        self.modal = Some(Modal::SaveAll(pending));
                    } else {
                        for k in &pending {
                            self.write_draft(k);
                        }
                    }
                }
            });
        });
        if let Some(p) = go {
            self.tm().unwrap().page = p;
            self.reload();
        }
        ui.add_space(4.0);
        self.results(ui, false);
    }

    pub(crate) fn columns_menu(&mut self, ui: &mut egui::Ui, agg: bool) {
        let key = self.col_key(agg);
        let t = self.t().unwrap();
        let fields = if agg { t.agg_fields.clone() } else { t.fields.clone() };
        let prefs = self.prefs.cols.get(&key).cloned().unwrap_or_default();
        let n = self.prefs.default_cols;
        let shown = visible_cols(Some(&prefs), &fields, n);
        let mut acts = Vec::new();
        let mut add = None;
        sticky_menu(ui, format!("Columns ({})", shown.len()), |ui| {
            ui.set_min_width(360.0);
            ui.add(egui::TextEdit::singleline(&mut self.col_filter).hint_text("Filter fields").desired_width(f32::INFINITY).margin(egui::Margin::symmetric(10, 6)));
            let needle = self.col_filter.trim().to_lowercase();
            let keep = |c: &str| needle.is_empty() || c.to_lowercase().contains(&needle);
            ui.add_space(6.0);
            caption(ui, "SHOWN  ·  drag to reorder, pinned columns stay first");
            egui::ScrollArea::vertical().id_salt("shown").max_height(320.0).show(ui, |ui| {
                for (i, c) in shown.iter().enumerate().filter(|(_, c)| keep(c)) {
                    let row = ui.horizontal(|ui| {
                        ui.set_min_height(28.0);
                        ui.dnd_drag_source(egui::Id::unique(("col-drag", c)), i, grip).response.on_hover_cursor(egui::CursorIcon::Grab);
                        let pinned = prefs.pinned.contains(c);
                        if pin_button(ui, pinned).on_hover_text(if pinned { "Unpin" } else { "Pin: keep this column first" }).clicked() {
                            acts.push(ColAct::Pin(c.clone()));
                        }
                        ui.label(RichText::new(c).monospace().color(if fields.contains(c) { TEXT() } else { PRIMARY() }));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, "🗙", true).on_hover_text("Hide").clicked() {
                                acts.push(ColAct::Hide(c.clone()));
                            }
                            if icon_button(ui, "⏷", i + 1 < shown.len()).on_hover_text("Move down").clicked() {
                                acts.push(ColAct::Move(c.clone(), 1));
                            }
                            if icon_button(ui, "⏶", i > 0).on_hover_text("Move up").clicked() {
                                acts.push(ColAct::Move(c.clone(), -1));
                            }
                        });
                    }).response;
                    // Drop on the upper half to go before this row, the lower half to go after it.
                    let below = ui.input(|inp| inp.pointer.hover_pos()).is_some_and(|p| p.y > row.rect.center().y);
                    if row.dnd_hover_payload::<usize>().is_some() {
                        let y = if below { row.rect.bottom() } else { row.rect.top() };
                        ui.painter().hline(row.rect.x_range(), y, Stroke::new(2.0, PRIMARY()));
                    }
                    if let Some(from) = row.dnd_release_payload::<usize>() {
                        acts.push(ColAct::MoveTo(shown[*from].clone(), i + below as usize));
                    }
                }
            });
            let hidden: Vec<&String> = fields.iter().filter(|f| !shown.contains(f) && keep(f)).collect();
            if !hidden.is_empty() {
                ui.separator();
                caption(ui, format!("MORE FIELDS ({})", hidden.len()));
                egui::ScrollArea::vertical().id_salt("more").max_height(200.0).show(ui, |ui| {
                    for f in hidden {
                        if ui.add(quiet_button(RichText::new(format!("+ {f}")).monospace()).min_size(egui::vec2(ui.available_width(), 26.0))).clicked() {
                            acts.push(ColAct::Show(f.clone()));
                        }
                    }
                });
            }
            ui.separator();
            caption(ui, "CUSTOM FIELD");
            ui.horizontal(|ui| {
                let w = ui.available_width() - 64.0;
                let r = ui.add(egui::TextEdit::singleline(&mut self.new_col).hint_text("customer.name").desired_width(w).font(egui::TextStyle::Monospace).margin(egui::Margin::symmetric(10, 6)));
                if (ui.add(egui::Button::new("Add").min_size(egui::vec2(56.0, BUTTON_H))).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))) && !self.new_col.trim().is_empty() {
                    add = Some(self.new_col.trim().to_string());
                }
            });
            // Nested paths seen in the loaded documents, for paths the field list does not offer.
            let typed = self.new_col.trim().to_lowercase();
            if !typed.is_empty() {
                let paths = if agg { &fields } else { &self.tabs[self.cur].paths };
                for path in paths.iter().filter(|x| x.to_lowercase().contains(&typed) && !shown.contains(x)).take(8) {
                    if ui.add(quiet_button(RichText::new(path).monospace()).min_size(egui::vec2(ui.available_width(), 24.0))).clicked() {
                        add = Some(path.clone());
                    }
                }
            }
            ui.label(RichText::new("Dotted paths reach inside objects and arrays, like items.0.sku").size(TXT_CAPTION).color(FAINT()));
            if ui.button(format!("Reset to the first {n} fields")).clicked() {
                acts.push(ColAct::Reset);
            }
        });
        if let Some(c) = add {
            acts.push(ColAct::Show(c));
            self.new_col.clear();
        }
        if !acts.is_empty() {
            self.apply_col_acts(&key, &fields, acts);
        }
    }

    pub(crate) fn apply_col_acts(&mut self, key: &str, fields: &[String], acts: Vec<ColAct>) {
        let n = self.prefs.default_cols;
        let p = self.prefs.cols.entry(key.to_string()).or_default();
        for a in acts {
            let mut shown = visible_cols(Some(p), fields, n);
            match a {
                ColAct::Highlight(c) => {
                    let c = db::strip_indices(&c);
                    if !p.highlight.contains(&c) {
                        p.highlight.push(c);
                    } else {
                        p.highlight.retain(|x| *x != c);
                    }
                    continue;
                }
                ColAct::Pin(c) => {
                    if !p.pinned.contains(&c) {
                        p.pinned.push(c);
                    } else {
                        p.pinned.retain(|x| *x != c);
                    }
                    continue;
                }
                ColAct::Hide(c) => {
                    shown.retain(|x| *x != c);
                    p.pinned.retain(|x| *x != c);
                }
                ColAct::Move(c, d) => {
                    if let Some(i) = shown.iter().position(|x| *x == c) {
                        let j = (i as i32 + d).clamp(0, shown.len() as i32 - 1) as usize;
                        let pinned_n = p.pinned.iter().filter(|x| shown.contains(x)).count();
                        shown.swap(i, j);
                        // Moving across the edge of the pinned block changes which columns are pinned.
                        p.pinned = shown[..pinned_n].to_vec();
                    }
                }
                ColAct::MoveTo(c, to) => {
                    if let Some(i) = shown.iter().position(|x| *x == c) {
                        let pinned_n = p.pinned.iter().filter(|x| shown.contains(x)).count();
                        let c = shown.remove(i);
                        shown.insert(if to > i { to - 1 } else { to }.min(shown.len()), c);
                        p.pinned = shown[..pinned_n].to_vec();
                    }
                }
                ColAct::Show(c) => {
                    if !shown.contains(&c) {
                        shown.push(c);
                    }
                }
                ColAct::Reset => {
                    *p = ColPrefs::default();
                    continue;
                }
            }
            p.shown = Some(shown);
        }
        self.save_prefs();
    }

    /// Table or JSON list of the current results, with the open document shown under its row.
    pub(crate) fn results(&mut self, ui: &mut egui::Ui, agg: bool) {
        // Following a reference while rendering can switch to another tab, so the docs go back by index.
        let cur = self.cur;
        let t = self.tm().unwrap();
        // Taken out for the frame so the open document can be edited through `self`.
        let docs = std::mem::take(if agg { &mut t.agg_docs } else { &mut t.docs });
        let fields = if agg { t.agg_fields.clone() } else { t.fields.clone() };
        let base = match (agg, t.view) {
            (true, View::Shell) => t.shell.page as u64 * PAGE_SIZE,
            (true, _) => 0,
            _ => t.page * PAGE_SIZE,
        };
        let loading = t.loading;
        let closed = self.t().and_then(|t| self.conn(t.sel.conn)).filter(|c| c.status != Status::Ready && !self.demo).map(|c| (c.meta.id, c.meta.name.clone(), c.status == Status::Connecting));
        if let (true, Some((id, name, connecting))) = (docs.is_empty(), closed) {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                if connecting {
                    spin(ui);
                    ui.label(RichText::new(format!("Connecting to {name}…")).color(MUTED()));
                    return;
                }
                ui.label(RichText::new(format!("Connect to {name} to load this tab.")).color(MUTED()));
                if primary(ui, "Connect", PRIMARY()).clicked() {
                    self.active = Some(id);
                    self.connect(id);
                }
            });
        } else if docs.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new(if loading { "Loading…" } else { "No documents" }).color(MUTED()));
        } else {
            if !agg {
                self.selection_bar(ui, &docs);
            }
            if self.prefs.table {
                self.doc_table(ui, agg, &docs, &fields, base);
            } else {
                self.doc_list(ui, agg, &docs);
            }
        }
        if let Some(t) = self.tabs.get_mut(cur) {
            *(if agg { &mut t.agg_docs } else { &mut t.docs }) = docs;
        }
    }

    /// Row click: Cmd toggles the row's check, Shift checks a range, a plain click opens it.
    pub(crate) fn row_click(&mut self, ui: &egui::Ui, agg: bool, i: usize) {
        let m = ui.input(|i| i.modifiers);
        if !agg && (m.command || m.shift) {
            self.toggle_check(i, m.shift);
        } else if self.prefs.open_inline {
            // Rows open and close on their own; the last one opened takes edits and keys.
            let t = self.tm().unwrap();
            let key = (agg, i);
            if t.expanded.remove(&key) || t.selected == Some(key) {
                self.close_row(key);
            } else {
                t.expanded.insert(key);
                self.select(agg, i, false);
            }
        } else {
            self.select(agg, i, true);
        }
    }

    pub(crate) fn close_row(&mut self, key: (bool, usize)) {
        let t = self.tm().unwrap();
        t.expanded.remove(&key);
        if t.selected == Some(key) {
            t.selected = None;
            self.reset_doc_state();
        }
    }

    pub(crate) fn toggle_check(&mut self, i: usize, range: bool) {
        let t = self.tm().unwrap();
        match t.check_anchor.filter(|_| range) {
            Some(a) => t.checked.extend(a.min(i)..=a.max(i)),
            None => {
                if !t.checked.insert(i) {
                    t.checked.remove(&i);
                }
            }
        }
        t.check_anchor = Some(i);
    }

    /// The documents a row action applies to: every checked one when the row is checked, else that row.
    pub(crate) fn act_targets(checked: &BTreeSet<usize>, docs: &[Document], i: usize) -> Vec<Document> {
        if checked.contains(&i) { Self::checked_docs(checked, docs) } else { docs.get(i).cloned().into_iter().collect() }
    }

    pub(crate) fn checked_docs(checked: &BTreeSet<usize>, docs: &[Document]) -> Vec<Document> {
        checked.iter().filter_map(|&j| docs.get(j).cloned()).collect()
    }

    pub(crate) fn selection_bar(&mut self, ui: &mut egui::Ui, docs: &[Document]) {
        let n = self.t().unwrap().checked.len();
        // Hidden under dialogs, which would otherwise sit behind it.
        if n == 0 || self.modal.is_some() || self.insert.is_some() {
            return;
        }
        let mut cancel = false;
        // Floats over the bottom of the results so checking rows never shifts the table.
        let area = ui.available_rect_before_wrap();
        let act = egui::Area::new(egui::Id::unique("selection-toolbar"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(egui::pos2(area.center().x, area.bottom() - 16.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(12, 8)).corner_radius(R_LG).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{n} selected")).strong().color(BRIGHT()));
                        ui.add_space(8.0);
                        let act = docs_menu(ui, n, false);
                        cancel = ui.button("Cancel").on_hover_text("Clear the selection").clicked();
                        act
                    })
                    .inner
                })
                .inner
            })
            .inner;
        if cancel {
            self.tm().unwrap().checked.clear();
        } else if let Some(a) = act {
            let sel = Self::checked_docs(&self.t().unwrap().checked, docs);
            self.docs_act(ui.ctx(), a, sel);
        }
    }

    pub(crate) fn docs_act(&mut self, ctx: &egui::Context, act: DocsAct, docs: Vec<Document>) {
        if docs.is_empty() {
            return;
        }
        let what = if docs.len() == 1 { "1 document".to_string() } else { format!("{} documents", docs.len()) };
        let value = || if docs.len() == 1 { Bson::Document(docs[0].clone()) } else { Bson::Array(docs.iter().cloned().map(Bson::Document).collect()) };
        let by_id = || {
            let mut ids: Vec<Bson> = docs.iter().filter_map(|d| d.get("_id").cloned()).collect();
            Bson::Document(if ids.len() == 1 { doc! { "_id": ids.remove(0) } } else { doc! { "_id": { "$in": ids } } })
        };
        match act {
            DocsAct::CopyJs => {
                ctx.copy_text(db::pretty_shell(&value()));
                self.ok(format!("Copied {what} as JS"));
            }
            DocsAct::CopyJson => {
                ctx.copy_text(db::to_edit_value(&value()));
                self.ok(format!("Copied {what} as JSON"));
            }
            DocsAct::CopyLines => {
                let lines: Vec<String> = docs.iter().map(|d| db::to_edit_line(&Bson::Document(d.clone()))).collect();
                ctx.copy_text(lines.join("\n"));
                self.ok(format!("Copied {what} as JSON lines"));
            }
            DocsAct::Duplicate => {
                let copies: Vec<Document> = docs.into_iter().map(|mut d| {
                    d.remove("_id");
                    d
                }).collect();
                // One copy opens the insert editor to tweak it first; many are written straight away.
                if let [c] = copies.as_slice() {
                    self.insert = Some(db::to_edit_json(c));
                } else if let Some((conn, target)) = self.writable_target() {
                    self.worker.import(conn, target, copies, None);
                }
            }
            DocsAct::Export => {
                let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                let file = rfd::FileDialog::new().set_file_name(format!("{coll}-selection.json")).add_filter("JSON", &["json"]).add_filter("CSV", &["csv"]).add_filter("TSV", &["tsv"]).save_file();
                let Some(path) = file else { return };
                let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("json").to_ascii_lowercase();
                match db::export_body(&docs, &ext).and_then(|b| std::fs::write(&path, b).map_err(|e| format!("Could not write {}: {e}", path.display()))) {
                    Ok(()) => self.ok(format!("Exported {what} to {}", path.display())),
                    Err(e) => self.err(e),
                }
            }
            DocsAct::Filter => {
                let t = self.tm().unwrap();
                t.filter_txt = db::shell_text(&by_id());
                t.page = 0;
                self.run_query();
            }
            DocsAct::Update => {
                let t = self.tm().unwrap();
                t.shell.code = format!(
                    "async function run() {{\n  return await {}.updateMany(\n    {},\n    {{ $set: {{  }} }}\n  );\n}}",
                    collection_ref(&t.sel.coll),
                    db::pretty_shell(&by_id())
                );
                t.view = View::Shell;
            }
            DocsAct::Delete => self.modal = Some(Modal::Delete(docs)),
        }
    }

    pub(crate) fn select(&mut self, agg: bool, i: usize, toggle: bool) {
        let t = self.tm().unwrap();
        let same = t.selected == Some((agg, i));
        t.selected = if same && toggle { None } else { Some((agg, i)) };
        if !same || toggle {
            self.reset_doc_state();
        }
    }

    pub(crate) fn doc_table(&mut self, ui: &mut egui::Ui, agg: bool, docs: &[Document], fields: &[String], base: u64) {
        let key = self.col_key(agg);
        let prefs = self.prefs.cols.get(&key).cloned().unwrap_or_default();
        let cols = visible_cols(Some(&prefs), fields, self.prefs.default_cols);
        let summarize = !self.prefs.object_preview;
        // Measuring lays out every loaded cell of the column, so it is done once per result set
        // (keyed by where the documents live and how many) instead of on every frame.
        let widths: Vec<f32> = cols
            .iter()
            .map(|c| {
                prefs.widths.get(c).copied().unwrap_or_else(|| {
                    let k = (docs.as_ptr() as usize, docs.len(), c.clone(), summarize);
                    if let Some(w) = self.width_cache.get(&k) {
                        return *w;
                    }
                    let w = optimal_width(ui, docs, c, summarize);
                    if self.width_cache.len() > 512 {
                        self.width_cache.clear();
                    }
                    self.width_cache.insert(k, w);
                    w
                })
            })
            .collect();
        // The documents view adds a checkbox in front of the row number.
        let num_w = if agg { NUM_W } else { NUM_W + CHECK_W };
        let total: f32 = num_w + widths.iter().sum::<f32>();
        let selected = self.t().unwrap().selected;
        let checked = if agg { BTreeSet::new() } else { self.t().unwrap().checked.clone() };
        let expanded = self.t().unwrap().expanded.clone();
        let mut check_click = None;
        let mut check_all = None;
        let mut doc_act = None;
        let view_w = ui.available_width();
        let mut click = None;
        let mut edit_cell = None;
        let mut col_acts = Vec::new();
        let mut resized = None;
        let mut save_widths = false;
        let mut fit = None;
        let mut sort_set: Option<(String, Option<i64>)> = None;
        let mut cell_acts: Vec<(usize, Vec<TreeAct>)> = Vec::new();
        // Single-field sort shown in the header; clicking cycles ascending, descending, off.
        let sort = if agg { None } else { self.t().and_then(|t| t.query.sort.clone()).filter(|s| s.len() == 1) };
        let dir_of = |c: &str| sort.as_ref().and_then(|s| s.get(c)).and_then(|v| match v {
            Bson::Int32(n) => Some(*n as i64),
            Bson::Int64(n) => Some(*n),
            Bson::Double(f) => Some(*f as i64),
            _ => None,
        });
        let hl_cols: Vec<bool> = cols.iter().map(|c| prefs.highlight.contains(&db::strip_indices(c))).collect();
        let related = if agg { HashMap::new() } else { self.t().map(|t| t.related.clone()).unwrap_or_default() };
        egui::Frame::new().fill(PANEL()).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(4, 4)).show(ui, |ui| {
            let mut area = egui::ScrollArea::both().id_salt(("res", agg)).auto_shrink(false);
            // QUARRY_SCROLL_TEST slides the table sideways every frame, to measure scrolling cost.
            if std::env::var("QUARRY_SCROLL_TEST").is_ok() {
                area = area.horizontal_scroll_offset((ui.ctx().cumulative_frame_nr() % 60) as f32 * 25.0);
            }
            area.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let row_w = total.max(view_w - 12.0);
                // Header
                let (hr, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::hover());
                let p = ui.painter();
                p.text(egui::pos2(hr.left() + 10.0, hr.center().y), egui::Align2::LEFT_CENTER, "#", FontId::proportional(TXT_CAPTION), FAINT());
                if !agg {
                    let all = checked.len() == docs.len();
                    let r = check_box(ui, egui::pos2(hr.left() + 13.0, hr.center().y), ui.scope_id().with("check-all"), all, !all && !checked.is_empty());
                    if r.on_hover_text("Select every document on this page").clicked() {
                        check_all = Some(!all);
                    }
                }
                let mut x = hr.left() + num_w;
                for (c, w) in cols.iter().zip(&widths) {
                    let cell = egui::Rect::from_min_size(egui::pos2(x, hr.top()), egui::vec2(*w, hr.height()));
                    let pinned = prefs.pinned.contains(c);
                    let color = if pinned { WARNING() } else if fields.contains(c) { MUTED() } else { PRIMARY() };
                    let dir = dir_of(c);
                    let hresp = ui.interact(cell.shrink2(egui::vec2(4.0, 0.0)), ui.scope_id().with(("h", c)), Sense::click());
                    if hresp.hovered() && !agg {
                        ui.painter().rect_filled(cell.shrink(2.0), R_SM, HOVER());
                    }
                    ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0))).text(egui::pos2(x + 8.0, cell.center().y), egui::Align2::LEFT_CENTER, c, FontId::proportional(TXT_BODY), if dir.is_some() { BRIGHT() } else { color });
                    if let Some(d) = dir {
                        let a = egui::pos2(cell.right() - 14.0, cell.center().y);
                        let pts = if d > 0 { vec![a + egui::vec2(-4.0, 2.5), a + egui::vec2(4.0, 2.5), a + egui::vec2(0.0, -3.0)] } else { vec![a + egui::vec2(-4.0, -2.5), a + egui::vec2(4.0, -2.5), a + egui::vec2(0.0, 3.0)] };
                        ui.painter().add(egui::Shape::convex_polygon(pts, PRIMARY(), Stroke::NONE));
                    }
                    let hresp = if agg { hresp.on_hover_text("Right-click for column options") } else { hresp.on_hover_text("Click to sort, right-click for column options") };
                    if hresp.clicked() && !agg {
                        let next = match dir {
                            None => Some(1),
                            Some(d) if d > 0 => Some(-1),
                            _ => None,
                        };
                        sort_set = Some((c.clone(), next));
                    }
                    hresp.context_menu(|ui| {
                        // Same three column actions as the original's table (renderer line 106057).
                        if !agg {
                            if ui.button("Sort ascending").clicked() {
                                sort_set = Some((c.clone(), Some(1)));
                            }
                            if ui.button("Sort descending").clicked() {
                                sort_set = Some((c.clone(), Some(-1)));
                            }
                            if ui.add_enabled(dir.is_some(), egui::Button::new("Unsort")).clicked() {
                                sort_set = Some((c.clone(), None));
                            }
                            ui.separator();
                        }
                        if ui.button(if pinned { "Unpin" } else { "Pin to the left" }).clicked() {
                            col_acts.push(ColAct::Pin(c.clone()));
                        }
                        if ui.button("Move left").clicked() {
                            col_acts.push(ColAct::Move(c.clone(), -1));
                        }
                        if ui.button("Move right").clicked() {
                            col_acts.push(ColAct::Move(c.clone(), 1));
                        }
                        if ui.button("Hide column").clicked() {
                            col_acts.push(ColAct::Hide(c.clone()));
                        }
                        ui.separator();
                        if ui.checkbox(&mut prefs.highlight.contains(&db::strip_indices(c)), "Highlight column").clicked() {
                            col_acts.push(ColAct::Highlight(c.clone()));
                            ui.close();
                        }
                    });
                    // Drag the right edge to resize.
                    let handle = egui::Rect::from_min_max(egui::pos2(cell.right() - 3.0, cell.top() + 6.0), egui::pos2(cell.right() + 3.0, cell.bottom() - 6.0));
                    let dr = ui.interact(handle, ui.scope_id().with(("w", c)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeColumn).on_hover_text("Drag to resize, double-click to fit");
                    if dr.double_clicked() {
                        fit = Some(c.clone());
                    }
                    ui.painter().vline(cell.right(), handle.y_range(), Stroke::new(1.0, if dr.hovered() || dr.dragged() { PRIMARY() } else { BORDER() }));
                    if dr.dragged() {
                        resized = Some((c.clone(), (*w + dr.drag_delta().x).clamp(50.0, 900.0)));
                    }
                    if dr.drag_stopped() {
                        save_widths = true;
                    }
                    x += w;
                }
                ui.painter().hline(hr.x_range(), hr.bottom(), Stroke::new(1.0, BORDER()));
                // Rows (at most one page, so no virtualisation is needed)
                for (i, d) in docs.iter().enumerate() {
                    let drafted = if agg { None } else { self.shown_doc(d) };
                    let d = drafted.as_ref().unwrap_or(d);
                    let active = selected == Some((agg, i));
                    let (rr, resp) = ui.allocate_exact_size(egui::vec2(row_w, ROW_H), Sense::click());
                    if active && std::mem::take(&mut self.scroll_sel) {
                        resp.scroll_to_me(None);
                    }
                    // Rows scrolled out of view keep their space but skip all layout and painting.
                    if ui.is_rect_visible(rr) {
                        let fill = if active {
                            tint(PRIMARY(), 32)
                        } else if checked.contains(&i) {
                            tint(PRIMARY(), 18)
                        } else if ui.rect_contains_pointer(rr) && !ui.ctx().any_popup_open() {
                            HOVER()
                        } else if i % 2 == 1 {
                            pal().stripe
                        } else {
                            Color32::TRANSPARENT
                        };
                        let p = ui.painter();
                        p.rect_filled(rr, R_SM, fill);
                        let off = num_w - NUM_W;
                        if self.prefs.open_inline {
                            chevron(p, egui::pos2(rr.left() + off + 9.0, rr.center().y), active || expanded.contains(&(agg, i)), if active { PRIMARY() } else { FAINT() });
                        }
                        let num = (base + i as u64 + 1).to_string();
                        let (num, nc) = if drafted.is_some() { (format!("{num}●"), WARNING()) } else { (num, FAINT()) };
                        p.text(egui::pos2(rr.left() + off + 18.0, rr.center().y), egui::Align2::LEFT_CENTER, num, FontId::proportional(TXT_CAPTION), nc);
                        if !agg && check_box(ui, egui::pos2(rr.left() + 13.0, rr.center().y), ui.scope_id().with(("check", i)), checked.contains(&i), false).clicked() {
                            check_click = Some(i);
                        }
                        let mut x = rr.left() + num_w;
                        for (ci, (c, w)) in cols.iter().zip(&widths).enumerate() {
                            let cell = egui::Rect::from_min_size(egui::pos2(x, rr.top()), egui::vec2(*w, ROW_H));
                            // Same for columns scrolled out sideways.
                            if !ui.is_rect_visible(cell) {
                                x += w;
                                continue;
                            }
                            if hl_cols[ci] {
                                ui.painter().rect_filled(cell, 0, HIGHLIGHT());
                            }
                            let clip = ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0)));
                            let v = db::get_path(d, c);
                            match v {
                                Some(v) => {
                                    let mut job = column_job(v, summarize);
                                    // Ends in an ellipsis at the cell's padding instead of being cut mid-glyph by the clip.
                                    job.wrap = egui::text::TextWrapping::truncate_at_width(w - 14.0);
                                    let g = ui.fonts_mut(|f| f.layout_job(job));
                                    let gw = g.size().x;
                                    clip.galley(egui::pos2(x + 8.0, cell.center().y - g.size().y / 2.0), g, TEXT());
                                    // Ids only, checked before compact(): that turns the whole value into JSON,
                                // which is costly for big objects on every frame.
                                let rel = (!related.is_empty() && matches!(v, Bson::ObjectId(_) | Bson::String(_))).then(|| related.get(&db::compact(v))).flatten();
                                if let Some(p) = rel {
                                        clip.text(egui::pos2(x + 14.0 + gw, cell.center().y), egui::Align2::LEFT_CENTER, p, FontId::proportional(TXT_SMALL), C_LINK());
                                    }
                                }
                                None => {
                                    clip.text(egui::pos2(x + 8.0, cell.center().y), egui::Align2::LEFT_CENTER, "—", FontId::proportional(TXT_SMALL), FAINT());
                                }
                            }
                            let cr = ui.interact(cell, ui.scope_id().with(("c", i, ci)), Sense::click());
                            if cr.double_clicked() {
                                edit_cell = Some((i, c.clone()));
                            } else if cr.clicked() {
                                click = Some(i);
                            }
                            let mut acts = Vec::new();
                            if let Some(v) = v {
                                let path: Vec<String> = c.split('.').map(String::from).collect();
                                cr.context_menu(|ui| {
                                    field_menu(ui, &path, v, !agg, hl_cols[ci], &mut acts);
                                    if !agg {
                                        ui.separator();
                                        doc_act = row_menu(ui, i, &checked).or(doc_act);
                                    }
                                });
                            } else if !agg {
                                cr.context_menu(|ui| {
                                    if ui.button("Edit full document as JSON…").clicked() {
                                        acts.push(TreeAct::EditDoc);
                                    }
                                    ui.separator();
                                    doc_act = row_menu(ui, i, &checked).or(doc_act);
                                });
                            }
                            if !acts.is_empty() {
                                cell_acts.push((i, acts));
                            }
                            x += w;
                        }
                    }
                    if resp.clicked() {
                        click = Some(i);
                    }
                    if !agg {
                        resp.context_menu(|ui| doc_act = row_menu(ui, i, &checked).or(doc_act));
                    }
                    if (active || expanded.contains(&(agg, i))) && self.prefs.open_inline {
                        ui.add_space(4.0);
                        // Spans every column like the rows above it, not just the visible part.
                        let w = row_w.max(320.0);
                        card(ui, |ui| {
                            // Card inner margin and stroke on both sides.
                            ui.set_width(w - 30.0);
                            ui.spacing_mut().item_spacing.y = 6.0;
                            self.doc_body(ui, d, agg, Some((agg, i)));
                        });
                        ui.add_space(6.0);
                    }
                }
            });
        });
        if let Some((c, w)) = resized {
            self.prefs.cols.entry(key.clone()).or_default().widths.insert(c, w);
        }
        if let Some(c) = fit {
            // Back to the measured width, like the original's "optimal" column width.
            self.prefs.cols.entry(key.clone()).or_default().widths.remove(&c);
            save_widths = true;
        }
        if save_widths {
            self.save_prefs();
        }
        if !col_acts.is_empty() {
            self.apply_col_acts(&key, fields, col_acts);
        }
        if let Some(i) = click {
            self.row_click(ui, agg, i);
        }
        if let Some(i) = check_click {
            self.toggle_check(i, ui.input(|i| i.modifiers.shift));
        }
        if let Some(on) = check_all {
            self.tm().unwrap().checked = if on { (0..docs.len()).collect() } else { BTreeSet::new() };
        }
        if let Some((i, a)) = doc_act {
            self.docs_act(&ui.ctx().clone(), a, Self::act_targets(&checked, docs, i));
        }
        for (i, acts) in cell_acts {
            self.select(agg, i, false);
            let d = self.shown_doc(&docs[i]).unwrap_or_else(|| docs[i].clone());
            self.handle_acts(&d, acts);
        }
        if let Some((c, next)) = sort_set {
            let t = self.tm().unwrap();
            t.query.sort = next.map(|d| doc! { c.clone(): d });
            t.sort_txt = t.query.sort.as_ref().map(|s| db::shell_text(&Bson::Document(s.clone()))).unwrap_or_default();
            t.page = 0;
            self.reload();
        }
        if let (Some((i, c)), false) = (edit_cell, agg) {
            self.select(agg, i, false);
            let path: Vec<String> = c.split('.').map(String::from).collect();
            let d = self.shown_doc(&docs[i]).unwrap_or_else(|| docs[i].clone());
            match db::get_path(&d, &c).cloned() {
                Some(v) if c != "_id" => self.start_inline(&d, path, v),
                Some(_) => {}
                None => self.err(format!("This document has no field {c}. Use + Add field.")),
            }
        }
    }

    pub(crate) fn doc_list(&mut self, ui: &mut egui::Ui, agg: bool, docs: &[Document]) {
        let selected = self.t().unwrap().selected;
        let checked = if agg { BTreeSet::new() } else { self.t().unwrap().checked.clone() };
        let expanded = self.t().unwrap().expanded.clone();
        let mut doc_act = None;
        let view_w = ui.available_width();
        let mut click = None;
        egui::Frame::new().fill(PANEL()).corner_radius(R_MD).inner_margin(4).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("list", agg)).auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, d) in docs.iter().enumerate() {
                    let drafted = if agg { None } else { self.shown_doc(d) };
                    let d = drafted.as_ref().unwrap_or(d);
                    let active = selected == Some((agg, i));
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
                    if active && std::mem::take(&mut self.scroll_sel) {
                        resp.scroll_to_me(None);
                    }
                    let fill = if active {
                        tint(PRIMARY(), 32)
                    } else if checked.contains(&i) {
                        tint(PRIMARY(), 18)
                    } else if resp.hovered() {
                        HOVER()
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, R_SM, fill);
                    let galley = ui.fonts_mut(|f| f.layout_job(row_job(d, w - 16.0)));
                    ui.painter().galley(rect.left_center() + egui::vec2(8.0, -galley.size().y / 2.0), galley, TEXT());
                    if resp.clicked() {
                        click = Some(i);
                    }
                    if !agg {
                        resp.context_menu(|ui| doc_act = row_menu(ui, i, &checked).or(doc_act));
                    }
                    if (active || expanded.contains(&(agg, i))) && self.prefs.open_inline {
                        ui.add_space(4.0);
                        card(ui, |ui| {
                            ui.set_width(view_w - 48.0);
                            ui.spacing_mut().item_spacing.y = 6.0;
                            self.doc_body(ui, d, agg, Some((agg, i)));
                        });
                        ui.add_space(6.0);
                    }
                }
            });
        });
        if let Some(i) = click {
            self.row_click(ui, agg, i);
        }
        if let Some((i, a)) = doc_act {
            self.docs_act(&ui.ctx().clone(), a, Self::act_targets(&checked, docs, i));
        }
    }

    pub(crate) fn save_to_library(&mut self, q: SavedQuery) {
        let key = self.col_key(false);
        let list = self.prefs.library.entry(key).or_default();
        let name = q.name.clone();
        list.retain(|x| !(x.name == q.name && x.pipeline.is_some() == q.pipeline.is_some()));
        list.push(q);
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.save_prefs();
        self.ok(format!("Saved \"{name}\" to the library"));
    }

    pub(crate) fn delete_from_library(&mut self, name: &str, pipeline: bool) {
        let key = self.col_key(false);
        if let Some(list) = self.prefs.library.get_mut(&key) {
            list.retain(|x| !(x.name == name && x.pipeline.is_some() == pipeline));
        }
        self.save_prefs();
    }

    pub(crate) fn export(&mut self) {
        let Some(target) = self.target() else { return self.err("Not connected.") };
        let file = rfd::FileDialog::new()
            .set_file_name(format!("{}.json", target.coll))
            .add_filter("JSON", &["json"])
            .add_filter("CSV", &["csv"])
            .add_filter("TSV", &["tsv"])
            .save_file();
        if let Some(path) = file {
            self.ok(format!("Exporting to {}…", path.display()));
            let q = self.t().unwrap().query.clone();
            self.worker.export(target, q, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_default_pin_and_move() {
        let fields: Vec<String> = (0..12).map(|i| format!("f{i}")).collect();
        assert_eq!(visible_cols(None, &fields, DEFAULT_COLS).len(), DEFAULT_COLS);
        let p = ColPrefs { pinned: vec!["f3".into()], ..Default::default() };
        assert_eq!(visible_cols(Some(&p), &fields, DEFAULT_COLS)[0], "f3");
        let p = ColPrefs { shown: Some(vec!["a".into(), "b".into()]), pinned: vec!["b".into(), "zz".into()], ..Default::default() };
        assert_eq!(visible_cols(Some(&p), &fields, DEFAULT_COLS), ["b", "a"]);
    }
}
