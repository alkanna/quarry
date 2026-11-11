//! The documents view: layout, row selection and actions on selected documents.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum DocsAct {
    CopyJs,
    CopyJson,
    CopyLines,
    /// A table or text format shared by Mongo documents and SQL rows.
    CopyAs(db::copy::Fmt),
    Duplicate,
    Export,
    Filter,
    Update,
    Delete,
    /// SQL rows: stage one value on every row.
    SetValue,
    /// SQL rows: open an UPDATE for these rows in the Query view.
    EditQuery,
}

/// Positions in `new` of the documents at `rows` in `old`, matched by _id.
pub(super) fn follow_ids(old: &[db::Row], new: &[db::Row], rows: impl Iterator<Item = usize>) -> BTreeSet<usize> {
    let ids: HashSet<String> = rows.filter_map(|i| old.get(i)?.id().map(|v| db::compact(&v))).collect();
    if ids.is_empty() {
        return BTreeSet::new();
    }
    new.iter().enumerate().filter(|(_, d)| d.id().is_some_and(|v| ids.contains(&db::compact(&v)))).map(|(i, _)| i).collect()
}

/// Actions on one document or the checked ones, shared by the selection bar and the row menus.
pub(super) fn docs_menu(ui: &mut egui::Ui, n: usize, heading: bool, sql: bool, grouped: bool) -> Option<DocsAct> {
    if heading {
        let (one, many) = if sql { ("Row".to_string(), format!("{n} selected rows")) } else { ("Document".to_string(), format!("{n} selected documents")) };
        ui.label(RichText::new(if n == 1 { one } else { many }).size(TXT_CAPTION).color(FAINT()));
    }
    let mut act = None;
    if let Some(a) = ui.menu_button("Copy as", |ui| copy_as_menu(ui, sql)).inner.flatten() {
        act = Some(a);
        ui.close();
    }
    let items: Vec<(&str, DocsAct)> = if sql {
        vec![("Duplicate", DocsAct::Duplicate), ("Export…", DocsAct::Export), ("Filter to these", DocsAct::Filter), ("Set value…", DocsAct::SetValue), ("Edit in Query", DocsAct::EditQuery), ("Delete…", DocsAct::Delete)]
    } else {
        vec![("Duplicate", DocsAct::Duplicate), ("Export…", DocsAct::Export), ("Filter to these", DocsAct::Filter), ("Update in shell", DocsAct::Update), ("Delete…", DocsAct::Delete)]
    };
    for (label, a) in items {
        let is_delete = matches!(a, DocsAct::Delete);
        if is_delete && sql {
            ui.separator();
        }
        let text = if is_delete { RichText::new(label).color(ALERT()) } else { RichText::new(label) };
        // Rows of a grouped answer are summaries, so these would only fail with a toast.
        let off = grouped && matches!(a, DocsAct::Duplicate | DocsAct::Delete | DocsAct::SetValue | DocsAct::EditQuery);
        if ui.add_enabled(!off, egui::Button::new(text)).on_disabled_hover_text("Grouped rows are summaries, not table rows. Ungroup to use this.").clicked() {
            act = Some(a);
        }
    }
    act
}

/// The floating bar over the bottom of the results while rows are checked: how many, the actions and Cancel.
/// Returns whether Cancel was pressed and the action picked.
pub(super) fn selection_area(ui: &mut egui::Ui, n: usize, hidden: bool, results_view: bool, sql: bool, grouped: bool) -> (bool, Option<DocsAct>) {
    if n == 0 || hidden {
        return (false, None);
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
                    ui.label(semibold(RichText::new(format!("{n} selected")).color(BRIGHT())));
                    ui.add_space(8.0);
                    let act = if results_view { results_menu(ui, n, false, sql) } else { docs_menu(ui, n, false, sql, grouped) };
                    cancel = ui.button("Cancel").on_hover_text("Clear the selection").clicked();
                    act
                })
                .inner
            })
            .inner
        })
        .inner;
    (cancel, act)
}

/// Actions on rows of a result that is only read: Query and Shell results. Copy as and Export.
pub(super) fn results_menu(ui: &mut egui::Ui, n: usize, heading: bool, sql: bool) -> Option<DocsAct> {
    if heading {
        let (one, many) = if sql { ("Row".to_string(), format!("{n} selected rows")) } else { ("Document".to_string(), format!("{n} selected documents")) };
        ui.label(RichText::new(if n == 1 { one } else { many }).size(TXT_CAPTION).color(FAINT()));
    }
    let mut act = ui.menu_button("Copy as", |ui| copy_as_menu(ui, sql)).inner.flatten();
    if act.is_some() {
        ui.close();
    }
    if ui.button("Export…").clicked() {
        act = Some(DocsAct::Export);
    }
    act
}

/// The row menu: acts on every checked document when the row is one of them, else on that row.
pub(super) fn row_menu(ui: &mut egui::Ui, i: usize, checked: &BTreeSet<usize>, sql: bool, grouped: bool) -> Option<(usize, DocsAct)> {
    menu_look(ui);
    docs_menu(ui, if checked.contains(&i) { checked.len() } else { 1 }, true, sql, grouped).map(|a| (i, a))
}

/// SQL clause names (ORDER BY, LIMIT) read as keywords; Mongo's lowercase option names and plain
/// words such as Columns stay faint.
pub(super) fn chip_name_color(name: &str) -> Color32 {
    if name.chars().any(|c| c.is_ascii_lowercase()) { FAINT() } else { C_KEYWORD() }
}

/// One option of the find bar: a pill holding its editor while it is set or being edited, else a
/// "+ name" chip that opens it. `edit` returns whether Enter was pressed. Returns whether the
/// query should run: on Enter, or when the cross removed the option.
pub(super) fn opt_chip(ui: &mut egui::Ui, name: &str, id: egui::Id, text: &mut String, edit: impl FnOnce(&mut egui::Ui, &mut String) -> bool) -> bool {
    let focused = ui.memory(|m| m.has_focus(id));
    if text.is_empty() && !focused {
        // The editor appears next frame; egui keeps a focus requested for a widget not yet drawn.
        if add_chip(ui, name).clicked() {
            ui.memory_mut(|m| m.request_focus(id));
        }
        return false;
    }
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let edge = if focused { PRIMARY() } else if hovered { BORDER() } else { LINE() };
    let (mut clear, mut enter) = (false, false);
    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, edge)).corner_radius(BUTTON_H / 2.0).inner_margin(egui::Margin { left: 10, right: 3, top: 0, bottom: 0 }).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Painted and centered on its ink like the other chips, because a label sits about 1 px high.
            let color = chip_name_color(name);
            let g = ui.painter().layout_no_wrap(name.to_string(), FontId::monospace(TXT_CAPTION), color);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(g.size().x, BUTTON_H), Sense::hover());
            let dy = optical_dy(&g);
            ui.painter().galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0 + dy), g, color);
            // Centered on the label: the editor is shorter than the chip.
            enter = ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| edit(ui, text)).inner;
            ui.allocate_ui_with_layout(egui::vec2(CLOSE_W, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                clear = close_x(ui).on_hover_text(format!("Remove {name}")).clicked();
            });
        });
    });
    if clear {
        text.clear();
    }
    clear || enter
}

/// Which menu a result row gets.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum RowMenu {
    Docs,
    /// SQL rows: the document actions with SQL's own.
    Sql,
    /// SQL rows of a GROUP BY answer, where the actions that write are off.
    SqlGrouped,
    Indexes,
    /// Rows of a result that is only read, such as the Shell's: Copy as and Export.
    Results(bool),
    None,
}

/// What a row menu asked for.
pub(super) enum RowAct {
    Doc(DocsAct),
    Idx(IdxAct),
}

/// The menu of row `i`: document actions for Documents rows, index actions for Indexes rows.
pub(super) fn any_row_menu(ui: &mut egui::Ui, kind: RowMenu, i: usize, rows: Rows, checked: &BTreeSet<usize>) -> Option<(usize, RowAct)> {
    match kind {
        RowMenu::Docs => row_menu(ui, i, checked, false, false).map(|(i, a)| (i, RowAct::Doc(a))),
        RowMenu::Sql => row_menu(ui, i, checked, true, false).map(|(i, a)| (i, RowAct::Doc(a))),
        RowMenu::SqlGrouped => row_menu(ui, i, checked, true, true).map(|(i, a)| (i, RowAct::Doc(a))),
        RowMenu::Results(sql) => {
            menu_look(ui);
            results_menu(ui, if checked.contains(&i) { checked.len() } else { 1 }, true, sql).map(|a| (i, RowAct::Doc(a)))
        }
        RowMenu::Indexes => index_menu(ui, rows.doc(i)?).map(|a| (i, RowAct::Idx(a))),
        RowMenu::None => None,
    }
}

impl App {
    pub(super) fn collection(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        // A Query tab that belongs to no table has no header or view tabs of a table.
        if t.sql.is_some() && t.sel.coll.is_empty() {
            return self.sql_query_view(ui);
        }
        let (coll, view, db) = (t.sel.coll.clone(), t.view, t.sel.scope_label().to_string());
        // What the collection holds: documents, data size and indexes. The whole collection's
        // count, not the filter's, which the results line shows. Without statistics only an
        // unfiltered count says what the collection holds.
        let mut stats = Vec::new();
        match t.stats {
            Some((count, size, nidx)) => {
                stats.push(format!("{} docs", group(&count.to_string())));
                stats.push(human_bytes(size as usize));
                stats.push(format!("{nidx} {}", if nidx == 1 { "index" } else { "indexes" }));
            }
            None if t.query.filter.is_empty() && t.query.limit.is_none() && t.query.skip == 0 => stats.push(format!("{} docs", group(&t.count.to_string()))),
            None => {}
        }
        let nidx = t.stats.map(|s| s.2);
        let sql = t.sql.is_some();
        if sql {
            stats.clear();
            if t.filter_txt.is_empty() {
                stats.push(format!("{} rows", group(&t.count.to_string())));
            }
        }
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new(&coll).size(TXT_HEADING).color(BRIGHT())));
            ui.add_space(6.0);
            ui.label(RichText::new(&db).font(FontId::monospace(TXT_SMALL)).color(FAINT()));
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), CHIP_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for s in stats.iter().rev() {
                    stat_chip(ui, s);
                }
            });
        });
        ui.add_space(4.0);
        // Wrapped, so on a narrow workspace the toggles move under the tabs instead of over them.
        let tabs = ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            for (v, label) in if sql { &SQL_VIEWS[..] } else { &MONGO_VIEWS[..] }.iter().copied() {
                if view_tab(ui, label, view == v).clicked() {
                    self.set_view(v);
                }
                if let (View::Indexes, Some(n)) = (v, nidx) {
                    ui.add_space(-16.0);
                    stat_chip(ui, &n.to_string());
                }
                if let (true, Some(n)) = (sql, self.sql_view_count(v)) {
                    ui.add_space(-16.0);
                    stat_chip(ui, &n.to_string());
                }
            }
            if sql && view == View::Documents {
                right_group(ui, |ui| self.columns_menu(ui, false));
            } else if !sql && matches!(view, View::Documents | View::Aggregate | View::Indexes | View::Watch) {
                ui.spacing_mut().item_spacing.x = 8.0;
                right_group(ui, |ui| {
                    if self.prefs.table {
                        self.columns_menu(ui, view != View::Documents);
                    }
                    let mut table = self.prefs.table;
                    segmented(ui, &mut table, &[(true, "Table"), (false, "JSON")]);
                    if table != self.prefs.table {
                        self.prefs.table = table;
                        self.save_prefs();
                    }
                });
            }
        });
        // A full-width rule under the tabs, which the active tab's underline sits on.
        let y = tabs.response.rect.bottom() - 0.5;
        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, LINE()));
        ui.add_space(12.0);
        match view {
            View::Documents if sql => self.sql_content(ui),
            View::Structure => self.sql_structure_view(ui),
            View::Relations => self.sql_relations_view(ui),
            View::Triggers => self.sql_triggers_view(ui),
            View::Info => self.sql_info_view(ui),
            View::Sql => self.sql_query_view(ui),
            View::Documents => self.documents(ui),
            View::Aggregate => self.aggregate(ui),
            View::Indexes => self.indexes_view(ui),
            View::Shell => self.shell_view(ui),
            View::Schema => self.schema_view(ui),
            View::Import => self.import_view(ui),
            View::Compare => self.compare_view(ui),
            View::Watch => self.watch_view(ui),
        }
    }

    pub(super) fn documents(&mut self, ui: &mut egui::Ui) {
        // Loaded with another page size, set from another tab: its page numbers no longer line up.
        let size = self.page_size();
        if let Some(t) = self.tm().filter(|t| t.size != 0 && t.size != size && !t.loading) {
            t.page = 0;
            self.reload();
        }
        let mut run = false;
        let (mut explain, mut help) = (false, false);
        let mut code_lang = None;
        let key = self.col_key(false);
        let hist = self.prefs.history.get(&key).cloned().unwrap_or_default();
        let index_sorts: Vec<String> = self.t().map(|t| t.indexes.iter().filter_map(|ix| ix.get_document("keys").ok()).map(|k| db::shell_text(&Bson::Document(k.clone()))).collect()).unwrap_or_default();
        let lib: Vec<SavedQuery> = library_of(&self.prefs.library, &key, LibKind::Query);
        let mut qacts = Vec::new();
        let (t, ac, new_lib) = (&mut self.tabs[self.cur], &mut self.ac, &mut self.new_lib);
        // Not asked yet (it is, right after this frame) or asked and not back.
        let (asked, waiting, notes) = (&self.values_asked, &self.values_pending, &self.values_note);
        let lookup = |p: &str| {
            let k = (key.clone(), p.to_string());
            if !asked.contains(&(k.0.clone(), k.1.clone(), None)) || waiting.contains(&k) {
                Lookup::Pending
            } else {
                notes.get(&k).map_or(Lookup::Done, |n| Lookup::Skipped(n))
            }
        };
        let paths = std::mem::take(&mut t.paths);
        let fields = std::mem::take(&mut t.field_info);
        let mut typed = Typed::default();
        let shell = db::is_shell(&t.filter_txt);
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.fonts_mut(|f| f.row_height(&mono)) + 4.0;
        // The bar is the filter's frame: it reads the filter's focus and hover from the last frame.
        let fid = query_id("QUERY");
        let focused = ui.memory(|m| m.has_focus(fid));
        let hovered = ui.ctx().read_response(fid).is_some_and(|r| r.hovered());
        let edge = if t.bar_err.is_some() { ALERT() } else if focused { PRIMARY() } else if hovered { MUTED() } else { BORDER() };
        let bar = egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, edge)).corner_radius(R_BAR).inner_margin(egui::Margin { left: 12, right: 6, top: 7, bottom: 7 }).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // Room for the history and library icons and Run, but never so little the box vanishes.
                let w = (ui.available_width() - 150.0).max(120.0);
                run |= Self::query_field(ui, "QUERY", &mut t.filter_txt, "{ field: value }   or paste db.coll.find(...) / db.coll.aggregate([...])", w, FieldKind::Query, &paths, &fields, ac, &mut typed, &lookup);
                // A fixed height, or a right-to-left layout takes all the height left in the bar.
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_h), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if run_button(ui, "Run", "↵").on_hover_text("Enter").clicked() {
                        run = true;
                    }
                    vsep(ui, 6.0);
                    library_menu(ui, &lib, new_lib, LibKind::Query, true, &mut qacts);
                    history_menu(ui, &hist, &index_sorts, &mut qacts);
                });
            });
        });
        if focused {
            ui.painter().rect_stroke(bar.response.rect.expand(1.0), R_BAR + 1, Stroke::new(2.0, tint(PRIMARY(), 60)), egui::StrokeKind::Outside);
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            // A pasted shell query carries its own sort, projection, limit and skip.
            if !shell {
                let fit = |ui: &egui::Ui, s: &str, hint: &str| {
                    let s = if s.is_empty() { hint } else { s.lines().max_by_key(|l| l.len()).unwrap_or(s) };
                    (text_w(ui, s, mono.clone()) + 8.0).clamp(24.0, 420.0)
                };
                // Some(kind) is a query box with completion; hint gets the sort box's, since index
                // keys are written like a sort. None is a plain one-line box.
                for (name, label, kind, text, hint) in [
                    ("sort", "SORT", Some(FieldKind::Sort), &mut t.sort_txt, "{ _id: -1 }"),
                    ("project", "PROJECT", Some(FieldKind::Project), &mut t.proj_txt, "{ name: 1 }"),
                    ("limit", "limit", None, &mut t.limit_txt, "0"),
                    ("skip", "skip", None, &mut t.skip_txt, "0"),
                    ("hint", "HINT", Some(FieldKind::Sort), &mut t.hint_txt, "{ field: 1 } or index name"),
                    ("collation", "COLLATION", None, &mut t.collation_txt, "{ locale: 'fr', strength: 2 }"),
                ] {
                    let id = query_id(label);
                    run |= opt_chip(ui, name, id, text, |ui, text| {
                        let w = fit(ui, text, hint);
                        if let Some(kind) = kind {
                            return Self::query_field(ui, label, text, hint, w, kind, &paths, &Default::default(), ac, &mut Typed::default(), &|_| Lookup::Done);
                        }
                        let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text(hint).desired_width(w).layouter(&mut code_layout));
                        if r.changed() && matches!(name, "limit" | "skip") {
                            text.retain(|c| c.is_ascii_digit());
                        }
                        r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                    });
                }
            }
            right_group(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                help |= quiet(ui, "?").on_hover_text("Query shortcuts and syntax").clicked();
                code_menu(ui, &mut code_lang);
                explain |= quiet(ui, "Explain").on_hover_text("Cmd+E").clicked();
            });
        });
        t.paths = paths;
        t.field_info = fields;
        // A lookup sent after this frame's list was drawn needs another frame to show its spinner;
        // otherwise the next frame comes only with the answer.
        let mut sent = false;
        if let Some(f) = &typed.field {
            sent |= self.want_values(key.clone(), f.clone(), None);
        }
        // Typing in a string asks for the values starting with it, once typing pauses.
        let now = std::time::Instant::now();
        if let Some(prefix) = typed.edit {
            let due = now + std::time::Duration::from_millis(400);
            self.values_typed = typed.field.zip(prefix).filter(|(_, p)| p.chars().count() >= 2).map(|(f, p)| (due, key.clone(), f, p));
        }
        if let Some((_, k, f, p)) = self.values_typed.take_if(|(due, ..)| *due <= now) {
            sent |= self.want_values(k, f, Some(p));
        } else if let Some((due, ..)) = &self.values_typed {
            ui.ctx().request_repaint_after(*due - now);
        }
        if sent {
            ui.ctx().request_repaint();
        }
        for a in qacts {
            let t = &mut self.tabs[self.cur];
            match a {
                QAct::Filter(v) => t.filter_txt = v,
                QAct::Sort(v) => t.sort_txt = v,
                QAct::Project(v) => t.proj_txt = v,
                QAct::Load(q) => {
                    t.clear_options();
                    (t.filter_txt, t.sort_txt, t.proj_txt) = (q.filter, q.sort, q.project);
                    run = true;
                }
                QAct::Save(name) => {
                    let q = SavedQuery { name: name.clone(), filter: t.filter_txt.clone(), sort: t.sort_txt.clone(), project: t.proj_txt.clone(), pipeline: None, sql: None };
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
        if help {
            self.modal = Some(Modal::Text { title: "Query help".into(), body: query_help(), code: false });
        }
        if let Some(l) = code_lang {
            self.show_code(l, true);
        }
        // Nothing was sent, so the rows below are still those of the last query that ran.
        if let Some(e) = self.t().and_then(|t| t.bar_err.clone()) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
                paint_alert(ui.painter(), r.center(), ALERT());
                ui.add(egui::Label::new(RichText::new(e).size(TXT_SMALL).color(TEXT())).wrap());
            });
        }
        ui.add_space(10.0);
        let t = self.t().unwrap();
        let (count, ms, loading, page) = (t.count, t.ms, t.loading, t.page);
        let pages = count.div_ceil(self.page_size()).max(1);
        let mut go: Option<u64> = None;
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new(format!("{} docs", group(&count.to_string()))).font(FontId::monospace(TXT_SMALL)).color(BRIGHT())));
            ui.label(RichText::new(format!("{ms} ms")).font(FontId::monospace(TXT_SMALL)).color(SUCCESS()));
            if loading {
                spin(ui);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                dots_menu(ui, |ui| {
                    if ui.button("Open query in Shell").clicked() {
                        let t = &self.tabs[self.cur];
                        self.open_in_shell(query_script(&t.sel.coll, &t.filter_txt));
                        ui.close();
                    }
                    let t = self.t().unwrap();
                    let (coll, scope, reach) = (t.sel.coll.clone(), t.query.scope(), t.bulk_count());
                    if ui.button("Update matching…").clicked() {
                        self.modal = Some(Modal::UpdateMany { set_txt: "{\n  \n}".into(), unset_txt: String::new(), preview: None });
                        ui.close();
                    }
                    ui.separator();
                    if danger_item(ui, "Delete all matching documents…").clicked() {
                        self.modal = Some(Modal::Typed {
                            title: format!("Delete every document in {coll} matching the current filter ({reach})?"),
                            expect: coll.clone(),
                            typed: String::new(),
                            action: Danger::DeleteMatching(scope),
                        });
                        ui.close();
                    }
                    if danger_item(ui, "Drop collection…").clicked() {
                        self.modal = Some(Modal::Typed {
                            title: format!("Drop the collection {coll} with all its documents and indexes? This cannot be undone."),
                            expect: coll,
                            typed: String::new(),
                            action: Danger::DropCollection,
                        });
                        ui.close();
                    }
                })
                .on_hover_text("More actions");
                if quiet(ui, "Export").on_hover_text("Export these documents").clicked() {
                    self.export();
                }
                if quiet(ui, "+ Insert").clicked() {
                    self.insert = Some("{\n  \n}".into());
                }
                vsep(ui, 4.0);
                if icon_button(ui, "⟳", true).on_hover_text("Reload (Cmd+R)").clicked() {
                    go = Some(page);
                }
                // The pager reads left to right, so it gets its own layout at last frame's width.
                let id = ui.scope_id().with("pager-w");
                let w = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(150.0);
                let r = ui.allocate_ui_with_layout(egui::vec2(w, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| pager(ui, page as usize, pages as usize));
                if r.response.rect.width() != w {
                    ui.data_mut(|d| d.insert_temp(id, r.response.rect.width()));
                    ui.ctx().request_repaint();
                }
                if let Some(p) = r.inner {
                    go = Some(p as u64);
                }
                let mut size = self.page_size();
                ui.scope(|ui| {
                    quiet_look(ui);
                    egui::ComboBox::from_id_salt("page-size").selected_text(format!("{size} / page")).show_ui(ui, |ui| {
                        menu_look(ui);
                        for n in [db::PAGE_SIZE, 100, 200, db::PAGE_SIZE_MAX] {
                            ui.selectable_value(&mut size, n, format!("{n} per page"));
                        }
                    });
                });
                // Remembered for every tab; the others reload when shown, see the top of this function.
                if size != self.page_size() {
                    self.prefs.page_size = size;
                    self.save_prefs();
                    go = Some(0);
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
        let size = self.page_size();
        let t = self.tm().unwrap();
        // Taken out of where the view keeps them for the frame, so the open document can be edited through `self`.
        let view = t.view;
        let (mut all, mut raw) = (Vec::new(), Vec::new());
        let (fields, base, range, is_raw) = if agg {
            let (_, fields, page, _) = t.agg_source();
            let fields = fields.to_vec();
            let is_raw = match t.agg_store(view) {
                Some(Store::Docs(v)) => {
                    all = std::mem::take(v);
                    false
                }
                Some(Store::Raw(v)) => {
                    raw = std::mem::take(v);
                    true
                }
                None => false,
            };
            let (range, base) = page_window(page, if is_raw { raw.len() } else { all.len() });
            (fields, base, range, is_raw)
        } else {
            raw = std::mem::take(&mut t.docs);
            (t.fields.clone(), t.page * size, 0..raw.len(), true)
        };
        // Rows that closed drop their decoded copy; only open ones keep it (see `db::Row`).
        if is_raw {
            for (i, r) in raw[range.clone()].iter_mut().enumerate() {
                if t.selected != Some((agg, i)) && !t.expanded.contains(&(agg, i)) {
                    r.forget();
                }
            }
        }
        let docs = if is_raw { Rows::Raw(&raw[range]) } else { Rows::Docs(&all[range]) };
        let loading = t.loading;
        let closed = self.t().and_then(|t| self.conn(t.sel.conn)).filter(|c| c.status != Status::Ready && !self.demo).map(|c| (c.meta.id, c.meta.name.clone(), c.status == Status::Connecting));
        let conn_failed = closed.as_ref().is_some_and(|c| self.conn_card_shown(c.0));
        let stale = !agg && self.t().is_some_and(|t| t.bar_err.is_some());
        if let (true, true, Some((id, ..))) = (docs.is_empty(), conn_failed, &closed) {
            self.conn_failed_card(ui, *id);
        } else if docs.is_empty() && self.failed_panel(ui, view) {
        } else if let (true, Some((id, name, connecting))) = (docs.is_empty(), closed) {
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
            let sql = self.t().is_some_and(|t| t.sql.is_some());
            if !agg || view == View::Shell {
                self.selection_bar(ui, docs);
            }
            ui.scope(|ui| {
                if stale {
                    ui.multiply_opacity(0.45);
                }
                if self.prefs.table || sql {
                    self.doc_table(ui, agg, docs, &fields, base);
                } else {
                    self.doc_list(ui, agg, docs);
                }
            });
        }
        if let Some(t) = self.tabs.get_mut(cur) {
            match (agg, t.agg_store(view)) {
                (false, _) => t.docs = raw,
                (true, Some(Store::Docs(v))) => *v = all,
                (true, Some(Store::Raw(v))) => *v = raw,
                (true, None) => {}
            }
        }
    }

    /// Row click: Cmd toggles the row's check, Shift checks a range, a plain click opens it.
    pub(super) fn row_click(&mut self, ui: &egui::Ui, agg: bool, i: usize) {
        let m = ui.input(|i| i.modifiers);
        if (!agg || self.t().is_some_and(|t| t.view == View::Shell)) && (m.command || m.shift) {
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

    /// The checked rows of the grid the current view shows: Content, a Query result or the Shell's result.
    pub(super) fn checks(&self) -> &BTreeSet<usize> {
        let t = self.t().unwrap();
        match (t.view, t.sql.as_ref()) {
            (View::Shell, _) => &t.shell_checked,
            (View::Sql, Some(s)) => &s.qchecked,
            _ => &t.checked,
        }
    }

    pub(super) fn checks_mut(&mut self) -> (&mut BTreeSet<usize>, &mut Option<usize>) {
        let t = self.tm().unwrap();
        match (t.view, t.sql.as_mut()) {
            (View::Shell, _) => (&mut t.shell_checked, &mut t.check_anchor),
            (View::Sql, Some(s)) => (&mut s.qchecked, &mut s.qanchor),
            _ => (&mut t.checked, &mut t.check_anchor),
        }
    }

    pub(super) fn toggle_check(&mut self, i: usize, range: bool) {
        let (checked, anchor) = self.checks_mut();
        match anchor.filter(|_| range) {
            Some(a) => checked.extend(a.min(i)..=a.max(i)),
            None => {
                if !checked.insert(i) {
                    checked.remove(&i);
                }
            }
        }
        *anchor = Some(i);
    }

    /// The documents a row action applies to: every checked one when the row is checked, else that row.
    pub(super) fn act_targets(checked: &BTreeSet<usize>, docs: Rows, i: usize) -> Vec<Document> {
        if checked.contains(&i) { Self::checked_docs(checked, docs) } else { docs.owned(i).into_iter().collect() }
    }

    pub(super) fn checked_docs(checked: &BTreeSet<usize>, docs: Rows) -> Vec<Document> {
        checked.iter().filter_map(|&j| docs.owned(j)).collect()
    }

    pub(super) fn selection_bar(&mut self, ui: &mut egui::Ui, docs: Rows) {
        let n = self.checks().len();
        // Hidden under dialogs, which would otherwise sit behind it.
        let hidden = self.modal.is_some() || self.insert.is_some();
        let sql = self.t().is_some_and(|t| t.sql.is_some());
        let grouped = self.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.matched.is_some());
        // Query and Shell results are read only: no edit or delete actions.
        let results_view = self.t().is_some_and(|t| matches!(t.view, View::Sql | View::Shell));
        let (cancel, act) = selection_area(ui, n, hidden, results_view, sql, grouped);
        if cancel {
            self.checks_mut().0.clear();
        } else if let Some(a) = act {
            let sel = Self::checked_docs(self.checks(), docs);
            self.docs_act(ui.ctx(), a, sel);
        }
    }

    pub(super) fn docs_act(&mut self, ctx: &egui::Context, act: DocsAct, docs: Vec<Document>) {
        if docs.is_empty() {
            return;
        }
        if self.t().is_some_and(|t| t.sql.is_some()) {
            return self.sql_docs_act(ctx, act, docs);
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
            DocsAct::CopyAs(f) => self.copy_rows_as(ctx, f, &docs),
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
            DocsAct::Export => self.open_export(docs, false),
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
            DocsAct::Delete => {
                if let Some(sel) = self.t().map(|t| t.sel.clone()) {
                    self.modal = Some(Modal::Delete(sel, docs));
                }
            }
            DocsAct::SetValue | DocsAct::EditQuery => {}
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
        let Some(target) = self.target() else { return self.warn_offline() };
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
