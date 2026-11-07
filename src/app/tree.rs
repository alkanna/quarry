//! One document as an editable tree: inline edits, drafts, field menus and the inspector.

use super::*;

pub(super) fn highlighted(prefs: &UiPrefs, key: &str, path: &[String]) -> bool {
    prefs.cols.get(key).is_some_and(|p| !p.highlight.is_empty() && p.highlight.contains(&db::strip_indices(&path.join("."))))
}

/// Right-click menu for one field, modelled on the original's fieldActions (renderer line 106562).
pub(super) fn fold_menu(ui: &mut egui::Ui, path: &[String], container: bool, acts: &mut Vec<TreeAct>) {
    ui.separator();
    if container && ui.button("Expand recursively").clicked() {
        acts.push(TreeAct::Fold(path.to_vec(), true));
    }
    if ui.button("Expand all").clicked() {
        acts.push(TreeAct::Fold(Vec::new(), true));
    }
    if ui.button("Collapse all").clicked() {
        acts.push(TreeAct::Fold(Vec::new(), false));
    }
}

pub(super) fn field_menu(ui: &mut egui::Ui, path: &[String], v: &Bson, editable: bool, highlighted: bool, acts: &mut Vec<TreeAct>) {
    let p = path.join(".");
    let is_id = path.first().is_some_and(|k| k == "_id");
    ui.set_min_width(230.0);
    ui.label(RichText::new(format!("Field: {p}")).size(TXT_SMALL).color(MUTED()));
    ui.separator();
    if ui.button("Copy value").clicked() {
        ui.ctx().copy_text(db::copy_text(v));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        ui.ctx().copy_text(p.clone());
        ui.close();
    }
    if ui.button("Show full value").clicked() {
        acts.push(TreeAct::ShowFull(p.clone(), v.clone()));
    }
    if editable && !is_id && ui.button("Edit value…").clicked() {
        acts.push(TreeAct::Start(path.to_vec(), v.clone()));
    }
    if editable && ui.button("Edit full document as JSON…").clicked() {
        acts.push(TreeAct::EditDoc);
    }
    if !is_id && matches!(v, Bson::ObjectId(_) | Bson::String(_)) && ui.button("Go to referenced document").on_hover_text("Opens the document whose _id is this value; the collection is found automatically the first time").clicked() {
        acts.push(TreeAct::GoRef(p.clone(), v.clone()));
    }
    ui.separator();
    if ui.button("Pin as a new column").clicked() {
        acts.push(TreeAct::PinColumn(p.clone()));
    }
    // A checkbox so an active highlight reads as on and is clearly undone from here.
    if ui.checkbox(&mut highlighted.clone(), "Highlight field").clicked() {
        acts.push(TreeAct::Highlight(p.clone()));
        ui.close();
    }
    ui.menu_button("Filter by value", |ui| {
        menu_look(ui);
        if ui.button("All documents").clicked() {
            acts.push(TreeAct::Filter(p.clone(), v.clone(), false));
        }
        if ui.button("Filtered documents only").clicked() {
            acts.push(TreeAct::Filter(p.clone(), v.clone(), true));
        }
    });
    ui.menu_button("Group by and count", |ui| {
        menu_look(ui);
        if ui.button("All documents").clicked() {
            acts.push(TreeAct::Group(p.clone(), v.clone(), false));
        }
        if ui.button("Filtered documents only").clicked() {
            acts.push(TreeAct::Group(p.clone(), v.clone(), true));
        }
    });
    if ui.button("Count distinct values").on_hover_text("Number of distinct values in this field within the current query").clicked() {
        acts.push(TreeAct::Distinct(db::strip_indices(&p)));
    }
    if ui.button("Create index on this field").clicked() {
        acts.push(TreeAct::Index(p.clone()));
    }
    if editable && !is_id {
        ui.separator();
        if ui.button("Rename field…").clicked() {
            acts.push(TreeAct::Rename(path.to_vec()));
        }
        if danger(ui, "Delete field").clicked() {
            acts.push(TreeAct::Remove(path.to_vec()));
        }
        if ui.button("Add field").on_hover_text("Add a field next to this one").clicked() {
            let parent = match v {
                Bson::Document(_) | Bson::Array(_) => path.to_vec(),
                _ => path[..path.len() - 1].to_vec(),
            };
            acts.push(TreeAct::AddField(parent));
        }
    }
    ui.separator();
    ui.label(RichText::new(format!("Field size: {} bytes", db::field_size(v))).size(TXT_SMALL).color(FAINT()));
}

impl App {
    /// Starts editing one value: small values in place, objects and arrays in the large JSON editor.
    pub(super) fn start_inline(&mut self, d: &Document, path: Vec<String>, v: Bson) {
        if !self.sel_writable() {
            let d = d.clone();
            return self.ask_unlock_then(move |a| a.start_inline(&d, path, v));
        }
        self.new_field = None;
        if matches!(v, Bson::Document(_) | Bson::Array(_)) {
            self.inline = None;
            self.json_edit = Some(JsonEdit { base: d.clone(), path, text: db::to_edit_value(&v), kind: db::type_name_kind(&v), focus: true });
        } else {
            self.inline = Some(Inline { text: db::value_text(&v), kind: db::type_name_kind(&v), path, focus: true });
        }
    }

    /// Side panel mode: the open document on the right.
    pub(super) fn inspector(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.selected_doc().cloned() else { return };
        let d = self.shown_doc(&d).unwrap_or(d);
        let from_agg = matches!(self.t().and_then(|t| t.selected), Some((true, _)));
        egui::Panel::right("inspector")
            .default_size(460.0)
            .size_range(320.0..=900.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(12))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.doc_body(ui, &d, from_agg, None));
            });
    }

    /// The open document: actions, then its fields as a tree that can be edited in place.
    /// `row` is the inline row showing this document, None in the side panel. Only the selected
    /// document holds edit state; acting in another open one selects it first.
    pub(super) fn doc_body(&mut self, ui: &mut egui::Ui, d: &Document, from_agg: bool, row: Option<(bool, usize)>) {
        let selected = self.t().and_then(|t| t.selected);
        let passive = row.is_some() && row != selected;
        // Edit state is keyed by field path only, so other open documents must not see it.
        let parked = passive.then(|| (self.inline.take(), self.new_field.take()));
        let mut acts = Vec::new();
        // The inline card can be wider than the table viewport; the header row stays within the
        // visible part so its buttons never sit behind the window edge.
        let full = ui.max_rect().x_range();
        let vis = ui.clip_rect().x_range();
        let (left, right) = (vis.min.max(full.min), vis.max.min(full.max) - 4.0);
        let w = (right - full.min).max(240.0);
        ui.allocate_ui_with_layout(egui::vec2(w, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
            ui.add_space((left - full.min).min(w - 240.0).max(0.0));
            let id = d.get("_id").map(db::short).unwrap_or_default();
            ui.label(RichText::new(id).monospace().color(C_OID()));
            ui.add_space(8.0);
            if !from_agg {
                if ui.button("✏ Edit JSON").clicked() {
                    acts.push(TreeAct::EditDoc);
                }
                if ui.button("✚ Add field").clicked() {
                    acts.push(TreeAct::AddField(Vec::new()));
                }
                if ui.button("🗋 Duplicate").clicked() {
                    self.docs_act(ui.ctx(), DocsAct::Duplicate, vec![d.clone()]);
                }
                if danger(ui, "🗑 Delete").clicked() {
                    // `d` carries unsaved edits; the delete only matches the document as stored.
                    let stored = self.drafts.get(&self.draft_key(d)).map(|dr| dr.original.clone()).unwrap_or_else(|| d.clone());
                    self.docs_act(ui.ctx(), DocsAct::Delete, vec![stored]);
                }
            }
            right_group(ui, |ui| {
                if ui.button("Close").on_hover_text("Esc").clicked() {
                    if let Some(key) = row.or(selected) {
                        self.close_row(key);
                    }
                }
                if ui.button("🗐 Copy JSON").clicked() {
                    self.docs_act(ui.ctx(), DocsAct::CopyJson, vec![d.clone()]);
                }
            });
        });
        ui.separator();
        let key = self.draft_key(d);
        self.changed = match self.drafts.get(&key) {
            Some(dr) if !from_agg => {
                let changes = db::diff(&dr.original, &dr.current);
                self.draft_bar(ui, &key, &changes);
                changes.into_iter().map(|c| c.0).collect()
            }
            _ => HashSet::new(),
        };
        let id = egui::Id::unique(("doc", d.get("_id").map(db::compact)));
        let mut path = Vec::new();
        self.fold_busy = false;
        // Runs every frame, so capped like doc_paths: deeper keys just truncate.
        fn longest(d: &Document, depth: usize) -> usize {
            d.iter().map(|(k, v)| k.chars().count().max(match v {
                _ if depth >= 4 => 0,
                Bson::Document(c) => longest(c, depth + 1),
                Bson::Array(a) => a.iter().take(20).filter_map(|v| v.as_document()).map(|c| longest(c, depth + 1)).max().unwrap_or(0),
                _ => 0,
            })).max().unwrap_or(0)
        }
        self.char_w = ui.fonts_mut(|f| f.glyph_width(&FontId::monospace(MONO_PX), 'x'));
        // Cached per document and field count, so open documents are not walked every frame.
        let cache = id.with(("key-chars", d.len()));
        let chars = ui.data(|m| m.get_temp::<usize>(cache)).unwrap_or_else(|| {
            let n = longest(d, 0).min(32);
            ui.data_mut(|m| m.insert_temp(cache, n));
            n
        });
        self.key_w = (chars as f32 * self.char_w + 24.0).clamp(96.0, 320.0);
        self.tree_x = ui.max_rect().x_range().expand(8.0);
        // Tree rows keep egui's compact 18 px; the global BUTTON_H would show a quarter fewer fields.
        let interact_h = std::mem::replace(&mut ui.spacing_mut().interact_size.y, 18.0);
        for (k, v) in d {
            path.push(k.clone());
            self.tree(ui, k, v, id, &mut path, !from_agg, &mut acts);
            path.pop();
        }
        ui.spacing_mut().interact_size.y = interact_h;
        // Nested headers only render once their parent opens, so the fold stays forced until every animation settles.
        if !self.fold_busy && self.fold.as_ref().is_some_and(|f| f.0 == id) {
            self.fold = None;
        }
        // Picked after this frame's headers ran, so applied after the clear above to reach the next frame.
        if let Some(TreeAct::Fold(p, open)) = acts.iter().find(|a| matches!(a, TreeAct::Fold(..))) {
            self.fold = Some((id, p.clone(), *open));
        }
        if self.new_field.as_ref().is_some_and(|n| n.parent.is_empty()) {
            self.new_field_row(ui, &mut acts);
        }
        self.changed.clear();
        if let Some((inline, new_field)) = parked {
            self.inline = inline;
            self.new_field = new_field;
        }
        if let (true, Some((agg, i)), false) = (passive, row, acts.is_empty()) {
            self.select(agg, i, false);
        }
        self.handle_acts(d, acts);
    }

    /// Unsaved edits of one document: what changed, Save, Discard, and the confirm switch.
    pub(super) fn draft_bar(&mut self, ui: &mut egui::Ui, key: &str, changes: &[(String, Option<Bson>, Option<Bson>)]) {
        let mut save = false;
        let mut discard = false;
        let mut confirm = self.prefs.confirm_save;
        egui::Frame::new().fill(tint(WARNING(), 22)).stroke(Stroke::new(1.0, WARNING().gamma_multiply(0.5))).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("● {} unsaved change(s)", changes.len())).color(WARNING()).strong());
                let list: Vec<&str> = changes.iter().take(5).map(|c| c.0.as_str()).collect();
                let more = if changes.len() > 5 { format!(" +{}", changes.len() - 5) } else { String::new() };
                ui.label(RichText::new(format!("{}{more}", list.join(", "))).monospace().size(TXT_SMALL).color(MUTED()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    save = primary(ui, "Save", PRIMARY()).on_hover_text("Write all edits of this document at once (Cmd+S)").clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(Key::S));
                    discard = ui.button("Discard").clicked();
                    if !self.prefs.always_writable {
                        ui.checkbox(&mut confirm, RichText::new("Ask before saving").size(TXT_SMALL));
                    }
                });
            });
        });
        ui.add_space(4.0);
        if confirm != self.prefs.confirm_save {
            self.prefs.confirm_save = confirm;
            self.save_prefs();
        }
        if discard {
            self.drafts.remove(key);
            self.ok("Edits discarded");
        } else if save {
            self.save_draft(key);
        }
    }

    pub(super) fn handle_acts(&mut self, d: &Document, acts: Vec<TreeAct>) {
        for a in acts {
            match a {
                // Applied by the document view, which knows the fold's id.
                TreeAct::Fold(..) => {}
                TreeAct::Start(p, v) => self.start_inline(d, p, v),
                TreeAct::Cancel => {
                    self.inline = None;
                    self.new_field = None;
                }
                TreeAct::Commit => {
                    if let Some(i) = &self.inline {
                        let new = db::make_value(i.kind, &i.text).and_then(|v| db::edit_path(d, &i.path, Some(v)));
                        self.stage(d, new);
                    }
                }
                TreeAct::Remove(p) => {
                    if self.sel_writable() {
                        self.stage(d, db::edit_path(d, &p, None));
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::Remove(p)]));
                    }
                }
                TreeAct::AddField(parent) => {
                    if self.sel_writable() {
                        self.inline = None;
                        self.new_field = Some(NewField { parent, name: String::new(), text: String::new(), kind: "string" });
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::AddField(parent)]));
                    }
                }
                TreeAct::CommitNew => {
                    if let Some(n) = &self.new_field {
                        let is_array = !n.parent.is_empty() && matches!(db::get_path(d, &n.parent.join(".")), Some(Bson::Array(_)));
                        let name = n.name.trim().to_string();
                        if name.is_empty() && !is_array {
                            self.err("Give the new field a name.");
                            continue;
                        }
                        let mut path = n.parent.clone();
                        path.push(if is_array { usize::MAX.to_string() } else { name.clone() });
                        if !is_array && db::get_path(d, &path.join(".")).is_some() {
                            self.err(format!("{name} already exists. Double-click it to change it."));
                            continue;
                        }
                        let new = db::make_value(n.kind, &n.text).and_then(|v| db::edit_path(d, &path, Some(v)));
                        self.stage(d, new);
                    }
                }
                TreeAct::EditDoc => {
                    if self.sel_writable() {
                        self.inline = None;
                        self.json_edit = Some(JsonEdit { base: d.clone(), path: Vec::new(), text: db::to_edit_json(d), kind: "object", focus: true });
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::EditDoc]));
                    }
                }
                TreeAct::ShowFull(path, v) => {
                    let body = match &v {
                        Bson::String(s) => s.clone(),
                        Bson::Document(_) | Bson::Array(_) => db::to_edit_json(&doc! { "v": v.clone() }).lines().skip(1).collect::<Vec<_>>().join("\n").trim_start().trim_start_matches("\"v\": ").trim_end_matches('}').trim_end().to_string(),
                        other => db::value_text(other),
                    };
                    self.modal = Some(Modal::Text { title: path, body, code: !matches!(v, Bson::String(_)) });
                }
                TreeAct::PinColumn(path) => {
                    let key = self.col_key(false);
                    let fields = self.t().map(|t| t.fields.clone()).unwrap_or_default();
                    self.apply_col_acts(&key, &fields, vec![ColAct::Show(path.clone()), ColAct::Pin(path)]);
                    self.prefs.table = true;
                }
                TreeAct::Highlight(path) => {
                    let fields = self.t().map(|t| t.fields.clone()).unwrap_or_default();
                    self.apply_col_acts(&self.col_key(false), &fields, vec![ColAct::Highlight(path)]);
                }
                TreeAct::Filter(path, v, add) => {
                    let path = db::strip_indices(&path);
                    let t = self.tm().unwrap();
                    let mut f = if add { t.query.filter.clone() } else { Document::new() };
                    f.insert(path, v);
                    t.filter_txt = db::shell_text(&Bson::Document(f));
                    t.view = View::Documents;
                    t.selected = None;
                    self.reset_doc_state();
                    self.run_query();
                }
                TreeAct::Group(path, v, filtered) => {
                    let filter = if filtered { self.t().unwrap().query.filter.clone() } else { Document::new() };
                    // Counted in the background and shown in a window, so the tab's own pipeline is kept;
                    // "Open in Aggregate" opens the pipeline in a new tab, as the original does.
                    match (db::group_count_pipeline(&path, &v, filter), self.target()) {
                        (Err(e), _) => self.err(e),
                        (_, None) => self.err("Not connected."),
                        (Ok(p), Some(target)) => {
                            let conn = self.t().unwrap().sel.conn;
                            self.ok(format!("Counting the values of {path}…"));
                            self.worker.group_count(conn, target, path, p);
                        }
                    }
                }
                TreeAct::Distinct(path) => match self.target() {
                    Some(target) => {
                        let f = self.t().unwrap().query.filter.clone();
                        self.worker.distinct_count(target, path, f);
                    }
                    None => self.err("Not connected."),
                },
                TreeAct::Index(path) => {
                    self.idx_keys = format!("{{ {}: 1 }}", db::shell_text(&Bson::Document(doc! { db::strip_indices(&path): 1 })).trim_start_matches("{ ").trim_end_matches(": 1 }"));
                    let t = self.tm().unwrap();
                    t.view = View::Indexes;
                    t.selected = None;
                    self.reset_doc_state();
                    self.load_indexes();
                    self.ok("Check the keys, then press Create (writes must be unlocked).");
                }
                TreeAct::GoRef(path, v) => {
                    let Some(s) = self.t().map(|t| t.sel.clone()) else { continue };
                    let path = db::strip_indices(&path);
                    let rel = self.prefs.relations.get(&format!("{}/{}", s.conn, s.db)).and_then(|l| l.iter().find(|r| r.coll == s.coll && r.path == path)).cloned();
                    match (rel, self.conn(s.conn).and_then(|c| c.client.clone())) {
                        (_, None) => self.err("Not connected."),
                        (Some(r), _) => self.go_ref(s.conn, s.db, r.target, v),
                        (None, Some(client)) => {
                            let colls = self.conn(s.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == s.db)).and_then(|d| d.1.clone()).unwrap_or_default();
                            self.ok("Looking for the collection that holds this id…");
                            self.worker.find_ref(s.conn, Target { client, db: s.db, coll: s.coll, timeout: self.timeout() }, colls, path, v);
                        }
                    }
                }
                TreeAct::Rename(path) => {
                    if self.sel_writable() {
                        let name = path.last().cloned().unwrap_or_default();
                        self.modal = Some(Modal::Rename { doc: d.clone(), path, name });
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::Rename(path)]));
                    }
                }
            }
        }
    }

    pub(super) fn type_combo(ui: &mut egui::Ui, id: &'static str, kind: &mut &'static str) {
        egui::ComboBox::from_id_salt(id).width(86.0).selected_text(RichText::new(*kind).size(TXT_SMALL)).show_ui(ui, |ui| {
            menu_look(ui);
            for k in db::KINDS {
                ui.selectable_value(kind, k, k);
            }
        });
    }

    pub(super) fn new_field_row(&mut self, ui: &mut egui::Ui, acts: &mut Vec<TreeAct>) {
        let Some(n) = &mut self.new_field else { return };
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let r = ui.add(egui::TextEdit::singleline(&mut n.name).hint_text("name").desired_width(120.0).font(egui::TextStyle::Monospace));
            if n.name.is_empty() && !r.has_focus() && !ui.ctx().any_popup_open() {
                r.request_focus();
            }
            let v = ui.add(egui::TextEdit::singleline(&mut n.text).hint_text("value").desired_width(EDITOR_W - 60.0).font(egui::TextStyle::Monospace));
            Self::type_combo(ui, "newkind", &mut n.kind);
            if ui.button("Add").clicked() || ((r.lost_focus() || v.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter))) {
                acts.push(TreeAct::CommitNew);
            }
            if ui.button("🗙").clicked() {
                acts.push(TreeAct::Cancel);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn tree(&mut self, ui: &mut egui::Ui, key: &str, v: &Bson, id: egui::Id, path: &mut Vec<String>, editable: bool, acts: &mut Vec<TreeAct>) {
        let editing_here = self.inline.as_ref().is_some_and(|i| i.path == *path);
        match v {
            Bson::Document(_) | Bson::Array(_) if !editing_here => {
                let items: Vec<(String, &Bson)> = match v {
                    Bson::Document(d) => d.iter().map(|(k, v)| (k.clone(), v)).collect(),
                    Bson::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
                    _ => unreachable!(),
                };
                let band = ui.painter().add(egui::Shape::Noop);
                let key_px = key.chars().count() as f32 * self.char_w;
                let mut job = LayoutJob::default();
                job.append(key, 0.0, mono(BRIGHT()));
                job.append(&leaf_text(v), (self.key_w - key_px).max(12.0), mono(FAINT()));
                let adding_here = self.new_field.as_ref().is_some_and(|n| n.parent == *path);
                let forced = self.fold.as_ref().filter(|f| f.0 == id && path.starts_with(&f.1)).map(|f| f.2);
                let r = egui::CollapsingHeader::new(job)
                    .id_salt(id.with(&*path))
                    .default_open(false)
                    .open(adding_here.then_some(true).or(forced))
                    .show(ui, |ui| {
                        for (k, child) in items {
                            path.push(k.clone());
                            self.tree(ui, &k, child, id, path, editable, acts);
                            path.pop();
                        }
                        if adding_here {
                            self.new_field_row(ui, acts);
                        }
                    });
                if highlighted(&self.prefs, &self.col_key(false), path) {
                    ui.painter().rect_filled(r.header_response.rect.expand(2.0), R_SM, HIGHLIGHT());
                }
                self.hover_band(ui, band, r.header_response.rect.y_range());
                if editable && r.header_response.double_clicked() && path.first().is_some_and(|k| k != "_id") {
                    acts.push(TreeAct::Start(path.clone(), v.clone()));
                }
                if forced.is_some_and(|o| r.openness != if o { 1.0 } else { 0.0 }) {
                    self.fold_busy = true;
                }
                let p = path.clone();
                let hl = highlighted(&self.prefs, &self.col_key(false), &p);
                r.header_response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Double-click to edit as JSON, right-click for more").context_menu(|ui| {
                    menu_look(ui);
                    field_menu(ui, &p, v, editable, hl, acts);
                    fold_menu(ui, &p, true, acts);
                });
            }
            leaf => {
                let hl = highlighted(&self.prefs, &self.col_key(false), path);
                let edited = !self.changed.is_empty() && self.changed.contains(&path.join("."));
                let band = ui.painter().add(egui::Shape::Noop);
                let row = egui::Frame::new().fill(if edited { tint(WARNING(), 34) } else if hl { HIGHLIGHT() } else { Color32::TRANSPARENT }).corner_radius(R_SM).show(ui, |ui| ui.horizontal(|ui| {
                    // Same offsets as a collapsing header so keys and values line up across both.
                    let x0 = ui.cursor().left();
                    ui.add_space(ui.spacing().indent);
                    let key_w = self.key_w;
                    let key_r = ui.scope(|ui| {
                        ui.set_max_width(key_w - 8.0);
                        ui.add(egui::Label::new(RichText::new(key).font(FontId::monospace(MONO_PX)).color(MUTED())).truncate().sense(Sense::click()))
                    }).inner;
                    ui.add_space((x0 + ui.spacing().indent + key_w - ui.cursor().left()).max(0.0));
                    if editing_here {
                        let inl = self.inline.as_mut().unwrap();
                        let r = ui.add(egui::TextEdit::singleline(&mut inl.text).desired_width(EDITOR_W).font(egui::TextStyle::Monospace).clip_text(true));
                        if inl.focus {
                            r.request_focus();
                            inl.focus = false;
                        }
                        Self::type_combo(ui, "inlinekind", &mut inl.kind);
                        if ui.button("✓").on_hover_text("Review and save (Enter)").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                            acts.push(TreeAct::Commit);
                        }
                        if ui.button("🗙").on_hover_text("Cancel (Esc)").clicked() {
                            acts.push(TreeAct::Cancel);
                        }
                    } else {
                        // A one-line label never shows more than this; laying out a megabyte string on
                        // every frame stalls scrolling. "Show full value" opens the whole thing.
                        const LEAF_MAX: usize = 2000;
                        let text = match leaf {
                            Bson::String(s) if s.len() > LEAF_MAX => format!("\"{}…\"", s.chars().take(LEAF_MAX).collect::<String>()),
                            _ => leaf_text(leaf),
                        };
                        let r = ui.add(egui::Label::new(RichText::new(&text).monospace().color(color_of(leaf))).truncate().sense(Sense::click()));
                        ui.label(RichText::new(db::type_name(leaf)).size(TXT_CAPTION).color(FAINT()));
                        let id_like = matches!(leaf, Bson::ObjectId(_)) || matches!(leaf, Bson::String(s) if s.len() <= 64);
                        if let Some(p) = self.t().filter(|t| id_like && !t.related.is_empty()).and_then(|t| t.related.get(&db::compact(leaf))) {
                            ui.label(RichText::new(p).size(TXT_SMALL).color(C_LINK())).on_hover_text("Preview of the referenced document");
                        }
                        if let Some(ts) = db::possible_timestamp(leaf) {
                            ui.label(RichText::new(format!("= {ts}?")).size(TXT_CAPTION).color(C_DATE().gamma_multiply(0.7))).on_hover_text("This number looks like a Unix timestamp in seconds");
                        }
                        let r = if editable {
                            let r = r.on_hover_cursor(egui::CursorIcon::Text).on_hover_text(if text.len() > 60 { text.chars().take(600).collect::<String>() } else { "Double-click to edit, right-click for more".into() });
                            if r.double_clicked() && path.first().is_some_and(|k| k != "_id") {
                                acts.push(TreeAct::Start(path.clone(), leaf.clone()));
                            }
                            r
                        } else {
                            r
                        };
                        let p = path.clone();
                        r.union(key_r).context_menu(|ui| {
                            menu_look(ui);
                            field_menu(ui, &p, leaf, editable, hl, acts);
                            fold_menu(ui, &p, false, acts);
                        });
                    }
                }));
                self.hover_band(ui, band, row.response.rect.y_range());
            }
        }
    }

    /// Paints a full-width band behind a tree row while the pointer is over it.
    pub(super) fn hover_band(&self, ui: &egui::Ui, at: egui::layers::ShapeIdx, y: egui::Rangef) {
        let r = egui::Rect::from_x_y_ranges(self.tree_x, y.expand(2.0));
        if ui.rect_contains_pointer(r) {
            ui.painter().set(at, egui::epaint::RectShape::filled(r, 4, HOVER()));
        }
    }
}
