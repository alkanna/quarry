//! The documents view: layout, row selection and actions on selected documents.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum DocsAct {
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
pub(super) fn follow_ids(old: &[Document], new: &[Document], rows: impl Iterator<Item = usize>) -> BTreeSet<usize> {
    let ids: HashSet<String> = rows.filter_map(|i| old.get(i)?.get("_id").map(db::compact)).collect();
    if ids.is_empty() {
        return BTreeSet::new();
    }
    new.iter().enumerate().filter(|(_, d)| d.get("_id").is_some_and(|v| ids.contains(&db::compact(v)))).map(|(i, _)| i).collect()
}

/// Actions on one document or the checked ones, shared by the selection bar and the row menus.
pub(super) fn docs_menu(ui: &mut egui::Ui, n: usize, heading: bool) -> Option<DocsAct> {
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
pub(super) fn row_menu(ui: &mut egui::Ui, i: usize, checked: &BTreeSet<usize>) -> Option<(usize, DocsAct)> {
    menu_look(ui);
    docs_menu(ui, if checked.contains(&i) { checked.len() } else { 1 }, true).map(|a| (i, a))
}

impl App {
    pub(super) fn collection(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn documents(&mut self, ui: &mut egui::Ui) {
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
            go = pager(ui, page as usize, pages as usize).map(|p| p as u64);
            if ui.button("⟳ Reload").clicked() {
                go = Some(page);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("More", |ui| {
                    menu_look(ui);
                    if ui.button("Open query in Shell").clicked() {
                        let t = &self.tabs[self.cur];
                        self.open_in_shell(query_script(&t.sel.coll, &t.filter_txt));
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

    /// Table or JSON list of the current results, with the open document shown under its row.
    pub(super) fn results(&mut self, ui: &mut egui::Ui, agg: bool) {
        // Following a reference while rendering can switch to another tab, so the docs go back by index.
        let cur = self.cur;
        let t = self.tm().unwrap();
        // Taken out of where the view keeps them for the frame, so the open document can be edited through `self`.
        let view = t.view;
        let (all, fields, range, base) = if agg {
            let (_, fields, page, _) = t.agg_source();
            let fields = fields.to_vec();
            let all = t.agg_store(view).map(std::mem::take).unwrap_or_default();
            let (range, base) = page_window(page, all.len());
            (all, fields, range, base)
        } else {
            let docs = std::mem::take(&mut t.docs);
            let range = 0..docs.len();
            (docs, t.fields.clone(), range, t.page * PAGE_SIZE)
        };
        let docs = &all[range];
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
                self.selection_bar(ui, docs);
            }
            if self.prefs.table {
                self.doc_table(ui, agg, docs, &fields, base);
            } else {
                self.doc_list(ui, agg, docs);
            }
        }
        if let Some(t) = self.tabs.get_mut(cur) {
            if !agg {
                t.docs = all;
            } else if let Some(store) = t.agg_store(view) {
                *store = all;
            }
        }
    }

    /// Row click: Cmd toggles the row's check, Shift checks a range, a plain click opens it.
    pub(super) fn row_click(&mut self, ui: &egui::Ui, agg: bool, i: usize) {
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

    pub(super) fn close_row(&mut self, key: (bool, usize)) {
        let t = self.tm().unwrap();
        t.expanded.remove(&key);
        if t.selected == Some(key) {
            t.selected = None;
            self.reset_doc_state();
        }
    }

    pub(super) fn toggle_check(&mut self, i: usize, range: bool) {
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
    pub(super) fn act_targets(checked: &BTreeSet<usize>, docs: &[Document], i: usize) -> Vec<Document> {
        if checked.contains(&i) { Self::checked_docs(checked, docs) } else { docs.get(i).cloned().into_iter().collect() }
    }

    pub(super) fn checked_docs(checked: &BTreeSet<usize>, docs: &[Document]) -> Vec<Document> {
        checked.iter().filter_map(|&j| docs.get(j).cloned()).collect()
    }

    pub(super) fn selection_bar(&mut self, ui: &mut egui::Ui, docs: &[Document]) {
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

    pub(super) fn docs_act(&mut self, ctx: &egui::Context, act: DocsAct, docs: Vec<Document>) {
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
                let coll = &self.t().unwrap().sel.coll;
                let code = format!(
                    "async function run() {{\n  return await {}.updateMany(\n    {},\n    {{ $set: {{  }} }}\n  );\n}}",
                    collection_ref(coll),
                    db::pretty_shell(&by_id())
                );
                self.open_in_shell(code);
            }
            DocsAct::Delete => self.modal = Some(Modal::Delete(docs)),
        }
    }

    pub(super) fn select(&mut self, agg: bool, i: usize, toggle: bool) {
        let t = self.tm().unwrap();
        let same = t.selected == Some((agg, i));
        t.selected = if same && toggle { None } else { Some((agg, i)) };
        if !same || toggle {
            self.reset_doc_state();
        }
    }

    pub(super) fn export(&mut self) {
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
