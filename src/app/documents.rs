//! The documents view: layout, row selection and actions on selected documents.

use super::results_bar::BarInfo;
use super::sql::bar::{line_frame, line_ring, LINE_GAP};
use super::*;
use quarry_core::action::{Action, BarField, DocsA, TabA};

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
    // Names the rows, so it is not mistaken for the cell's "Filter to this value".
    let what = if sql { "row" } else { "document" };
    let filter = if n == 1 { format!("Filter to this {what}") } else { format!("Filter to these {n} {what}s") };
    let items: Vec<(&str, DocsAct)> = if sql {
        vec![("Duplicate", DocsAct::Duplicate), ("Export…", DocsAct::Export), (&filter, DocsAct::Filter), ("Set value…", DocsAct::SetValue), ("Edit in Query", DocsAct::EditQuery), ("Delete…", DocsAct::Delete)]
    } else {
        vec![("Duplicate", DocsAct::Duplicate), ("Export…", DocsAct::Export), (&filter, DocsAct::Filter), ("Update in shell", DocsAct::Update), ("Delete…", DocsAct::Delete)]
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
    // A frame is a group of widgets, which egui does not move to the next row of a wrapping line by itself.
    flow_item(ui, id, |ui| egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, edge)).corner_radius(BUTTON_H / 2.0).inner_margin(egui::Margin { left: 10, right: 3, top: 0, bottom: 0 }).show(ui, |ui| {
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
    }));
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
        if t.sql.is_some() && t.sel.coll.is_empty() && t.view != View::Shell {
            return self.sql_query_view(ui);
        }
        let (coll, view, uid) = (t.sel.coll.clone(), t.view, t.uid);
        let nidx = t.stats.map(|s| s.2);
        let sql = t.sql.is_some();
        // The collection's name and place are in the tab and the breadcrumb, and its size is in the results bar.
        // Wrapped, so on a narrow workspace the toggles move under the tabs instead of over them.
        let tabs = ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            // A tab with no collection (a Shell script opened from the library) has only the views that need none.
            for (v, label) in if sql { &SQL_VIEWS[..] } else { &MONGO_VIEWS[..] }.iter().copied().filter(|(v, _)| !coll.is_empty() || matches!(v, View::Shell | View::Sql)) {
                if view_tab(ui, label, view == v).clicked() {
                    self.exec(Action::Tab(TabA::View { tab: uid, view: v }));
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
            // Documents and the SQL Content view carry Table | JSON and Columns in their results bar.
            if !sql && matches!(view, View::Aggregate | View::Indexes | View::Watch) {
                ui.spacing_mut().item_spacing.x = 8.0;
                right_group(ui, |ui| {
                    if self.core.prefs.table {
                        self.columns_menu(ui, true);
                    }
                    let mut table = self.core.prefs.table;
                    segmented(ui, &mut table, &[(true, "Table"), (false, "JSON")]);
                    if table != self.core.prefs.table {
                        self.exec(Action::Docs(DocsA::Layout { table }));
                    }
                });
            }
        });
        // A full-width rule under the tabs, which the active tab's underline sits on.
        let y = tabs.response.rect.bottom() - 0.5;
        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, LINE()));
        ui.add_space(12.0);
        self.script_banners(ui);
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
        self.snippet_strip(ui, crate::app::library::snippet_strip::Src::Bar);
        let mut run = false;
        let (mut explain, mut help) = (false, false);
        let mut code_lang = None;
        let mut demo_pick = None;
        let key = self.col_key(false);
        let hist = self.core.prefs.history.get(&key).cloned().unwrap_or_default();
        let index_sorts: Vec<String> = self.t().map(|t| t.indexes.iter().filter_map(|ix| ix.get_document("keys").ok()).map(|k| db::shell_text(&Bson::Document(k.clone()))).collect()).unwrap_or_default();
        let pick = self.pick_data(ui.ctx(), PickKind::Query);
        let (mut qacts, mut pick_acts) = (Vec::new(), Vec::new());
        let (t, ac) = (&self.core.tabs[self.core.cur], &mut self.ui.ac);
        // Not asked yet (it is, right after this frame) or asked and not back.
        let (asked, waiting, notes) = (&self.core.values_asked, &self.core.values_pending, &self.core.values_note);
        let lookup = |p: &str| {
            let k = (key.clone(), p.to_string());
            if !asked.contains(&(k.0.clone(), k.1.clone(), None)) || waiting.contains(&k) {
                Lookup::Pending
            } else {
                notes.get(&k).map_or(Lookup::Done, |n| Lookup::Skipped(n))
            }
        };
        let (paths, fields) = (&t.paths, &t.field_info);
        // The boxes are edited on copies and sent back as actions when they changed.
        let mut txt = [t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone(), t.limit_txt.clone(), t.skip_txt.clone(), t.hint_txt.clone(), t.collation_txt.clone()];
        let [filter_txt, sort_txt, proj_txt, limit_txt, skip_txt, hint_txt, collation_txt] = &mut txt;
        let mut typed = Typed::default();
        let shell = db::is_shell(&t.filter_txt);
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        // The bar is the filter's frame: it reads the filter's focus and hover from the last frame.
        let dirty = super::run::query_dirty(t);
        let (frame, focused) = line_frame(ui, query_id("QUERY"), t.bar_err.is_some(), dirty, 12, 6);
        let bar_err = t.bar_err.clone();
        let (mut set, mut picked) = ([false; 6], None);
        // The option chips: the name each shows, the label of its editor, and whether it is set.
        const OPTS: [(&str, &str); 6] = [("sort", "SORT"), ("project", "PROJECT"), ("limit", "limit"), ("skip", "skip"), ("hint", "HINT"), ("collation", "COLLATION")];
        for (i, text) in [&t.sort_txt, &t.proj_txt, &t.limit_txt, &t.skip_txt, &t.hint_txt, &t.collation_txt].into_iter().enumerate() {
            set[i] = !text.is_empty();
        }
        let bar = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = LINE_GAP;
                // The edited note, the history and library icons and Run with its tip. Drawn first, in a child over the first row
                // that claims no room, so the filter and the options get the width it leaves.
                let row = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), BUTTON_H));
                let mut right = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Min)));
                right.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    run |= super::sql::bar::line_run(ui, dirty, |ui| {
                        if super::sql::bar::run_item(ui, "Explain", "⌘E").on_hover_text("Explain the query that ran").clicked() {
                            explain = true;
                            ui.close();
                        }
                        ui.menu_button("Copy as code", |ui| {
                            menu_look(ui);
                            for l in db::Lang::ALL {
                                if ui.button(l.name()).clicked() {
                                    code_lang = Some(l);
                                    ui.close();
                                }
                            }
                        });
                    });
                    vsep(ui, 6.0);
                    let button = picker_button(ui, &pick, true, &mut pick_acts);
                    demo_pick.get_or_insert(button);
                    history_menu(ui, &hist, &index_sorts, None, &mut qacts);
                    edited_note(ui, dirty);
                });
                let left_w = (ui.available_width() - right.min_rect().width() - 16.0).max(240.0);
                ui.allocate_ui_with_layout(egui::vec2(left_w, BUTTON_H + 2.0), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                    ui.spacing_mut().item_spacing = LINE_GAP;
                    // As wide as its longest line, up to most of the line, so the options can follow it on the same row.
                    let hint = "{ field: value }   or paste db.coll.find(...) / db.coll.aggregate([...])";
                    let longest = if filter_txt.is_empty() { hint } else { filter_txt.lines().max_by_key(|l| l.len()).unwrap_or("") };
                    let w = (text_w(ui, longest, mono.clone()) + 24.0).clamp(220.0, (left_w * 0.6).max(220.0)).min(left_w);
                    run |= Self::query_field(ui, "QUERY", filter_txt, hint, w, FieldKind::Query, paths, fields, ac, &mut typed, &lookup);
                    // A pasted shell query carries its own sort, projection, limit and skip.
                    if shell {
                        return;
                    }
                    let fit = |ui: &egui::Ui, s: &str, hint: &str| {
                        let s = if s.is_empty() { hint } else { s.lines().max_by_key(|l| l.len()).unwrap_or(s) };
                        (text_w(ui, s, mono.clone()) + 8.0).clamp(24.0, 420.0)
                    };
                    // Some(kind) is a query box with completion; hint gets the sort box's, since index
                    // keys are written like a sort. None is a plain one-line box. An option shows as a chip while it is set
                    // or its editor has focus, which is how the "+" menu starts an empty one.
                    for (name, label, kind, text, hint) in [
                        ("sort", "SORT", Some(FieldKind::Sort), &mut *sort_txt, "{ _id: -1 }"),
                        ("project", "PROJECT", Some(FieldKind::Project), &mut *proj_txt, "{ name: 1 }"),
                        ("limit", "limit", None, &mut *limit_txt, "0"),
                        ("skip", "skip", None, &mut *skip_txt, "0"),
                        ("hint", "HINT", Some(FieldKind::Sort), &mut *hint_txt, "{ field: 1 } or index name"),
                        ("collation", "COLLATION", None, &mut *collation_txt, "{ locale: 'fr', strength: 2 }"),
                    ] {
                        let id = query_id(label);
                        if text.is_empty() && !ui.memory(|m| m.has_focus(id)) {
                            continue;
                        }
                        run |= opt_chip(ui, name, id, text, |ui, text| {
                            let w = fit(ui, text, hint);
                            if let Some(kind) = kind {
                                return Self::query_field(ui, label, text, hint, w, kind, paths, &Default::default(), ac, &mut Typed::default(), &|_| Lookup::Done);
                            }
                            let r = crate::ui::editor::show_paired(ui, egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text(hint).desired_width(w).layouter(&mut code_layout)).response;
                            if r.changed() && matches!(name, "limit" | "skip") {
                                text.retain(|c| c.is_ascii_digit());
                            }
                            r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                        });
                    }
                    let plus = super::sql::bar::add_button(ui);
                    let items: Vec<_> = OPTS.iter().zip(set).map(|((name, _), on)| super::sql::bar::AddItem { label: *name, note: "", group: "OPTIONS", on, keyword: false }).collect();
                    picked = super::sql::bar::add_menu(&plus, &items, &mut help);
                });
            });
            // Nothing was sent, so the rows below are still those of the last query that ran.
            if let Some(e) = &bar_err {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
                    paint_alert(ui.painter(), r.center(), ALERT());
                    ui.add(egui::Label::new(RichText::new(e.as_str()).size(TXT_SMALL).color(TEXT())).wrap());
                });
            }
        });
        if focused {
            line_ring(ui, bar.response.rect);
        }
        if let Some(name) = picked && let Some((_, label)) = OPTS.iter().find(|(n, _)| *n == name) {
            ui.memory_mut(|m| m.request_focus(query_id(label)));
        }
        if dirty {
            ui.add_space(8.0);
            run |= super::sql::bar::stale_note(ui);
        }
        let tab = t.uid;
        let bar_acts: Vec<DocsA> = [(BarField::Filter, &t.filter_txt), (BarField::Sort, &t.sort_txt), (BarField::Project, &t.proj_txt), (BarField::Limit, &t.limit_txt), (BarField::Skip, &t.skip_txt), (BarField::Hint, &t.hint_txt), (BarField::Collation, &t.collation_txt)]
            .into_iter()
            .zip(txt)
            .filter(|((_, old), new)| *old != new)
            .map(|((field, _), text)| DocsA::Bar { tab, field, text })
            .collect();
        for a in bar_acts {
            self.exec(Action::Docs(a));
        }
        // A lookup sent after this frame's list was drawn needs another frame to show its spinner;
        // otherwise the next frame comes only with the answer.
        let mut sent = false;
        if let Some(f) = &typed.field {
            sent |= self.want_values(key.clone(), f.clone(), None);
        }
        // Typing in a string asks for the values starting with it, once typing pauses (Core's tick waits it out).
        if let Some(prefix) = typed.edit {
            self.exec(Action::Docs(DocsA::TypedValues { key: key.clone(), field: typed.field.clone(), prefix }));
        }
        if sent {
            ui.ctx().request_repaint();
        }
        for a in qacts {
            let (field, text) = match a {
                QAct::Filter(v) => (BarField::Filter, v),
                QAct::Sort(v) => (BarField::Sort, v),
                QAct::Project(v) => (BarField::Project, v),
            };
            self.exec(Action::Docs(DocsA::History { tab, field, text }));
        }
        if let Some(b) = demo_pick {
            self.demo_open_picker(ui.ctx(), &b, "picker-docs");
        }
        if run {
            self.exec(Action::Docs(DocsA::Run { tab }));
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
        self.agent_notice(ui);
        ui.add_space(10.0);
        let t = self.t().unwrap();
        // The collection's data size and index count, which the header used to carry.
        let stats = t.stats.map(|(_, size, nidx)| [human_bytes(size as usize), format!("{nidx} {}", if nidx == 1 { "index" } else { "indexes" })]);
        let info = BarInfo { page: t.page, size: self.page_size(), total: t.count, unit: "docs", about: if t.count_approx { "at least " } else { "" }, ms: t.ms, loading: t.loading, stats: stats.as_ref().map_or(&[][..], |s| &s[..]), grouped: None };
        let out = self.results_bar(ui, &info, |app, ui| {
            let more = dots_menu(ui, |ui| {
                if ui.button("Export").on_hover_text("Export these documents").clicked() {
                    app.export();
                    ui.close();
                }
                if ui.button("Open query in Shell").clicked() {
                    let t = &app.core.tabs[app.core.cur];
                    // A pasted shell command carries its own options.
                    match super::run::find_query(t) {
                        Ok(q) => app.open_in_shell(query_script(&t.sel.coll, &t.filter_txt, &cursor_chain(&q))),
                        Err(_) if db::is_shell(t.filter_txt.trim().trim_end_matches(';')) => app.open_in_shell(query_script(&t.sel.coll, &t.filter_txt, "")),
                        Err(e) => app.warn(e),
                    }
                    ui.close();
                }
                let t = app.t().unwrap();
                let (coll, scope, reach) = (t.sel.coll.clone(), t.query.scope(), t.bulk_count());
                if ui.button("Update matching…").clicked() {
                    app.modal = Some(Modal::UpdateMany { set_txt: "{\n  \n}".into(), unset_txt: String::new(), preview: None });
                    ui.close();
                }
                ui.separator();
                if danger_item(ui, "Delete all matching documents…").clicked() {
                    app.modal = Some(Modal::Typed {
                        title: format!("Delete every document in {coll} matching the current filter ({reach})?"),
                        expect: coll.clone(),
                        typed: String::new(),
                        action: app.pinned(Danger::DeleteMatching(scope)),
                    });
                    ui.close();
                }
                if danger_item(ui, "Drop collection…").clicked() {
                    let sel = app.t().unwrap().sel.clone();
                    app.modal = Some(Modal::Typed {
                        title: format!("Drop the collection {coll} with all its documents and indexes? This cannot be undone."),
                        expect: coll,
                        typed: String::new(),
                        // Named here rather than pinned: the drop closes this tab, so the tab to come back to is gone.
                        action: Danger::DropCollAt { conn: sel.conn, db: sel.db, coll: sel.coll },
                    });
                    ui.close();
                }
            })
            .on_hover_text("More actions");
            // The button shows only painted dots, so screen readers and the UI tests need a name.
            a11y(&more, egui::Role::Button, "Document actions");
            if app.core.prefs.table {
                app.columns_menu(ui, false);
            }
            let mut table = app.core.prefs.table;
            segmented(ui, &mut table, &[(true, "Table"), (false, "JSON")]);
            if table != app.core.prefs.table {
                app.exec(Action::Docs(DocsA::Layout { table }));
            }
            if quiet(ui, "+ Insert").clicked() {
                app.exec(Action::Docs(DocsA::Insert { tab }));
            }
            let pending = app.tab_drafts();
            if !pending.is_empty() && primary(ui, &format!("Save all ({})", pending.len()), WARNING()).on_hover_text("Documents with unsaved edits").clicked() {
                app.exec(Action::Docs(DocsA::SaveAll { tab }));
            }
        });
        if let Some(page) = out.go {
            self.exec(Action::Docs(DocsA::Page { tab, page }));
        }
        ui.add_space(4.0);
        self.results(ui, false);
        // Last, since opening a script may change the tab this frame was drawn for.
        self.take_repaint(ui.ctx());
        self.pick_acts(pick_acts);
    }

    /// Table or JSON list of the current results, with the open document shown under its row.
    pub(super) fn results(&mut self, ui: &mut egui::Ui, agg: bool) {
        // Following a reference while rendering can switch to another tab, so the docs go back by index.
        let cur = self.core.cur;
        let size = self.page_size();
        let Some(t) = self.t() else { return };
        let view = t.view;
        // The window on screen is held for the frame, so the open document can be edited through `self`. Rows share their
        // bytes, so holding them copies none.
        let (all, raw, fields, base, is_raw): (Vec<Document>, Vec<db::Row>, Vec<String>, u64, bool) = if agg {
            let (rows, fields, base) = t.agg_page();
            match rows {
                Rows::Docs(d) => (d.to_vec(), Vec::new(), fields.to_vec(), base, false),
                Rows::Raw(r) => (Vec::new(), r.to_vec(), fields.to_vec(), base, true),
            }
        } else {
            (Vec::new(), t.docs.clone(), t.fields.clone(), t.page * size, true)
        };
        let docs = if is_raw { Rows::Raw(&raw[..]) } else { Rows::Docs(&all[..]) };
        let mut after = None;
        let loading = t.loading;
        let closed = self.t().and_then(|t| self.conn(t.sel.conn)).filter(|c| c.status != Status::Ready && !self.core.demo).map(|c| (c.meta.id, c.meta.name.clone(), c.status == Status::Connecting));
        let conn_failed = closed.as_ref().is_some_and(|c| self.conn_card_shown(c.0));
        let stale = !agg && self.t().is_some_and(|t| t.bar_err.is_some() || super::run::query_dirty(t));
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
                    self.exec(Action::Conn(ConnA::Switch { conn: id }));
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
                if self.core.prefs.table || sql {
                    after = Some(self.doc_table(ui, agg, docs, &fields, base));
                } else {
                    self.doc_list(ui, agg, docs);
                }
            });
        }
        // A link followed while drawing may have moved to another tab, whose rows these indexes are not.
        if let Some(a) = after.filter(|_| self.core.cur == cur) {
            self.sql_after(ui.ctx(), a);
        }
    }

    /// Row click: Cmd toggles the row's check, Shift checks a range, a plain click opens it.
    pub(super) fn row_click(&mut self, ui: &egui::Ui, agg: bool, i: usize) {
        let m = ui.input(|i| i.modifiers);
        let Some(tab) = self.t().map(|t| t.uid) else { return };
        self.exec(Action::Docs(DocsA::Click { tab, agg, row: i, cmd: m.command, shift: m.shift }));
    }

    pub(super) fn checks(&self) -> &BTreeSet<usize> {
        self.core.checks()
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
        let hidden = self.modal.is_some() || self.core.insert.is_some();
        let sql = self.t().is_some_and(|t| t.sql.is_some());
        let grouped = self.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.matched.is_some());
        // Query and Shell results are read only: no edit or delete actions.
        let results_view = self.t().is_some_and(|t| matches!(t.view, View::Sql | View::Shell));
        let (cancel, act) = selection_area(ui, n, hidden, results_view, sql, grouped);
        if cancel {
            let tab = self.t().unwrap().uid;
            self.exec(Action::Docs(DocsA::ClearChecks { tab }));
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
        let tab = self.t().unwrap().uid;
        match act {
            DocsAct::Duplicate => return self.exec(Action::Docs(DocsA::Duplicate { tab, docs })),
            DocsAct::Delete => return self.exec(Action::Docs(DocsA::Delete { tab, docs })),
            DocsAct::Filter => return self.exec(Action::Docs(DocsA::Filter { tab, docs })),
            _ => {}
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
            DocsAct::Export => self.open_export(docs, false),
            DocsAct::Update => {
                let coll = &self.t().unwrap().sel.coll;
                let code = format!(
                    "async function run() {{\n  return await {}.updateMany(\n    {},\n    {{ $set: {{  }} }}\n  );\n}}",
                    collection_ref(coll),
                    db::pretty_shell(&by_id())
                );
                self.open_in_shell(code);
            }
            DocsAct::Duplicate | DocsAct::Delete | DocsAct::Filter | DocsAct::SetValue | DocsAct::EditQuery => {}
        }
    }

    #[cfg(test)]
    pub(super) fn select(&mut self, agg: bool, i: usize, toggle: bool) {
        let Some(tab) = self.t().map(|t| t.uid) else { return };
        self.exec(Action::Docs(DocsA::Select { tab, agg, row: i, toggle }));
    }

    pub(super) fn export(&mut self) {
        let Some(target) = self.target() else { return self.warn_offline() };
        let tab = self.t().unwrap().uid;
        let file = rfd::FileDialog::new()
            .set_file_name(format!("{}.json", target.coll))
            .add_filter("JSON", &["json"])
            .add_filter("CSV", &["csv"])
            .add_filter("TSV", &["tsv"])
            .save_file();
        if let Some(path) = file {
            self.exec(Action::Mongo(quarry_core::action::MongoA::Import(quarry_core::action::ImportA::Export { tab, path })));
        }
    }
}
