//! The results table and list: columns, widths and painting rows.

use super::*;

/// The rows a results table draws: result documents, or the Documents view's rows kept as BSON bytes.
#[derive(Clone, Copy)]
pub(super) enum Rows<'a> {
    Docs(&'a [Document]),
    Raw(&'a [db::Row]),
}

impl<'a> Rows<'a> {
    pub(super) fn len(self) -> usize {
        match self {
            Rows::Docs(d) => d.len(),
            Rows::Raw(r) => r.len(),
        }
    }

    pub(super) fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub(super) fn slice(self, range: std::ops::Range<usize>) -> Rows<'a> {
        match self {
            Rows::Docs(d) => Rows::Docs(&d[range]),
            Rows::Raw(r) => Rows::Raw(&r[range]),
        }
    }

    /// Where the rows live, part of the column width cache key.
    fn addr(self) -> usize {
        match self {
            Rows::Docs(d) => d.as_ptr() as usize,
            Rows::Raw(r) => r.as_ptr() as usize,
        }
    }

    /// The value at a dotted path, read without decoding the rest of the row.
    pub(super) fn value(self, i: usize, path: &str) -> Option<Val<'a>> {
        match self {
            Rows::Docs(d) => db::get_path(d.get(i)?, path).map(Val::B),
            Rows::Raw(r) => r.get(i)?.value(path).map(Val::R),
        }
    }

    /// Row `i` decoded; a raw row keeps it until the row closes, so only call it for open rows.
    pub(super) fn doc(self, i: usize) -> Option<&'a Document> {
        match self {
            Rows::Docs(d) => d.get(i),
            Rows::Raw(r) => r.get(i).map(db::Row::doc),
        }
    }

    /// A decoded copy of row `i`, for actions.
    pub(super) fn owned(self, i: usize) -> Option<Document> {
        match self {
            Rows::Docs(d) => d.get(i).cloned(),
            Rows::Raw(r) => r.get(i).map(db::Row::owned),
        }
    }

    pub(super) fn id(self, i: usize) -> Option<Bson> {
        match self {
            Rows::Docs(d) => d.get(i)?.get("_id").cloned(),
            Rows::Raw(r) => r.get(i)?.id(),
        }
    }

    fn doc_ref(self, i: usize) -> DocRef<'a> {
        match self {
            Rows::Docs(d) => DocRef::B(&d[i]),
            Rows::Raw(r) => DocRef::from(&r[i]),
        }
    }
}

/// The original's "optimal" column width (measureColumnWidth, renderer line 31273): the widest
/// cell, kept between 40 and 400 px, plus 10 px. Our cells add 16 px of inner padding on top.
/// `header` is the text drawn in the header, a custom label or the path itself.
pub(super) fn optimal_width(ui: &egui::Ui, docs: Rows, col: &str, header: &str, summarize: bool, cap: f32) -> f32 {
    if docs.is_empty() {
        return 100.0 + CELL_PAD; // the original's fallback width
    }
    let widest = ui.fonts_mut(|f| {
        let head = f.layout_no_wrap(header.to_string(), FontId::proportional(TXT_BODY), TEXT()).size().x;
        (0..docs.len())
            .filter_map(|i| docs.value(i, col))
            .map(|v| f.layout_job(column_job(v, summarize)).size().x)
            .fold(head, f32::max)
    });
    widest.clamp(OPTIMAL_MIN_W, cap) + OPTIMAL_PAD + CELL_PAD
}

/// Left edge for text `gw` wide in a cell at `x`, `w` wide, by the column's alignment. Text wider
/// than the cell starts at its left padding so its beginning stays visible.
fn align_x(align: u8, x: f32, w: f32, gw: f32) -> f32 {
    match align {
        1 => x + (w - gw) / 2.0,
        2 => x + w - 8.0 - gw,
        _ => x + 8.0,
    }
    .max(x + 8.0)
}

pub(super) fn visible_cols(p: Option<&ColPrefs>, fields: &[String], n: usize) -> Vec<String> {
    let base: Vec<String> = match p.and_then(|p| p.shown.clone()) {
        Some(v) => v,
        None => fields.iter().take(n).cloned().collect(),
    };
    let pinned: Vec<String> = p.map(|p| p.pinned.clone()).unwrap_or_default();
    let mut out: Vec<String> = pinned.iter().filter(|c| base.contains(c)).cloned().collect();
    out.extend(base.into_iter().filter(|c| !pinned.contains(c)));
    out
}

impl App {
    pub(super) fn columns_menu(&mut self, ui: &mut egui::Ui, agg: bool) {
        let key = self.col_key(agg);
        let t = self.t().unwrap();
        let fields = if agg { t.agg_source().1.to_vec() } else { t.fields.clone() };
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
            // Nested paths seen in the loaded documents, for paths the field list does not offer.
            let paths: &[String] = if agg { &fields } else { &self.tabs[self.cur].paths };
            let suggest = |b: &str| {
                let typed = b.trim().to_lowercase();
                let items: Vec<Sug> = paths
                    .iter()
                    .filter(|p| !typed.is_empty() && p.to_lowercase().contains(&typed) && !shown.contains(p))
                    .take(50)
                    .map(|p| Sug { back: 0, text: p.clone(), note: "field", insert: p.clone(), caret: p.chars().count() })
                    .collect();
                (!items.is_empty()).then(|| (b.chars().count(), items))
            };
            let id = ui.make_persistent_id("custom-col");
            let mut moved = false;
            ui.horizontal(|ui| {
                let w = ui.available_width() - 64.0;
                let (out, picked, m) = ac_input(ui, id, &mut self.new_col, &mut self.ac, &suggest, |ui, text, _| {
                    egui::TextEdit::singleline(text).id(id).hint_text("customer.name").desired_width(w).font(egui::TextStyle::Monospace).margin(egui::Margin::symmetric(10, 6)).show(ui)
                });
                moved = m;
                let enter = out.response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui.add(egui::Button::new("Add").min_size(egui::vec2(56.0, BUTTON_H))).clicked() || enter || picked) && !self.new_col.trim().is_empty() {
                    add = Some(self.new_col.trim().to_string());
                }
            });
            // Drawn in the menu, not as a popup: a click on another layer would close the menu.
            if ac_list(ui, id, &mut self.new_col, &mut self.ac, moved, Some(ui.available_width() - 2.0)) {
                add = Some(self.new_col.trim().to_string());
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

    pub(super) fn apply_col_acts(&mut self, key: &str, fields: &[String], acts: Vec<ColAct>) {
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
                ColAct::Align(c, a) => {
                    if a == 0 {
                        p.align.remove(&c);
                    } else {
                        p.align.insert(c, a);
                    }
                    continue;
                }
                ColAct::Customize { col, label, path } => {
                    if path != col {
                        if shown.contains(&path) {
                            shown.retain(|x| *x != col);
                            p.pinned.retain(|x| *x != col);
                        } else {
                            shown.iter_mut().chain(p.pinned.iter_mut()).filter(|x| **x == col).for_each(|x| *x = path.clone());
                        }
                        // A column already at that path keeps its own width and alignment.
                        if let Some(w) = p.widths.remove(&col) {
                            p.widths.entry(path.clone()).or_insert(w);
                        }
                        if let Some(a) = p.align.remove(&col) {
                            p.align.entry(path.clone()).or_insert(a);
                        }
                        p.labels.remove(&col);
                        let (from, to) = (db::strip_indices(&col), db::strip_indices(&path));
                        if p.highlight.contains(&from) {
                            p.highlight.retain(|h| *h != from && *h != to);
                            p.highlight.push(to);
                        }
                    }
                    if label.trim().is_empty() {
                        p.labels.remove(&path);
                    } else {
                        p.labels.insert(path, label.trim().to_string());
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

    pub(super) fn doc_table(&mut self, ui: &mut egui::Ui, agg: bool, docs: Rows, fields: &[String], base: u64) {
        let key = self.col_key(agg);
        let prefs = self.prefs.cols.get(&key).cloned().unwrap_or_default();
        let cols = visible_cols(Some(&prefs), fields, self.prefs.default_cols);
        let indexed: Vec<Option<String>> = cols.iter().map(|c| self.t().and_then(|t| t.indexes_on(c)).map(str::to_string)).collect();
        let summarize = !self.prefs.object_preview;
        // Measuring lays out every loaded cell of the column, so it is done once per result set
        // (keyed by where the documents live and how many) instead of on every frame.
        let widths: Vec<f32> = cols
            .iter()
            .map(|c| {
                prefs.widths.get(c).copied().unwrap_or_else(|| {
                    // The header text is measured too, so a custom label is part of the key.
                    let k = (docs.addr(), docs.len(), c.clone(), prefs.labels.get(c).cloned(), summarize);
                    if let Some(w) = self.width_cache.get(&k) {
                        return *w;
                    }
                    let w = optimal_width(ui, docs, c, prefs.label(c), summarize, OPTIMAL_MAX_W);
                    if self.width_cache.len() > 512 {
                        self.width_cache.clear();
                    }
                    self.width_cache.insert(k, w);
                    w
                })
            })
            .zip(cols.iter().zip(&indexed))
            // A fitted width leaves room for the index bolt; a width the user dragged stays as set.
            .map(|(w, (c, ix))| if ix.is_none() || prefs.widths.contains_key(c) { w } else { w + BOLT_ROOM })
            .collect();
        // The documents view adds a checkbox in front of the row number.
        let num_w = if agg { NUM_W } else { NUM_W + CHECK_W };
        let total: f32 = num_w + widths.iter().sum::<f32>();
        let selected = self.t().unwrap().selected;
        let checked = if agg { BTreeSet::new() } else { self.t().unwrap().checked.clone() };
        let expanded = self.t().unwrap().expanded.clone();
        let mut check_click = None;
        let mut check_all = None;
        let mut row_act = None;
        let view_w = ui.available_width();
        let mut click = None;
        let mut edit_cell = None;
        let mut resized = None;
        let mut save_widths = false;
        let mut fit = None;
        let mut head_acts: Vec<(String, HeadAct)> = Vec::new();
        let mut scope_query = self.col_scope_query;
        let mut cell_acts: Vec<(usize, Vec<TreeAct>)> = Vec::new();
        // Single-field sort shown in the header and set from the column popover.
        let sort = if agg { None } else { self.t().and_then(|t| t.query.sort.clone()).filter(|s| s.len() == 1) };
        let dir_of = |c: &str| sort.as_ref().and_then(|s| s.get(c)).and_then(|v| match v {
            Bson::Int32(n) => Some(*n as i64),
            Bson::Int64(n) => Some(*n),
            Bson::Double(f) => Some(*f as i64),
            _ => None,
        });
        let aligns: Vec<u8> = cols.iter().map(|c| prefs.align.get(c).copied().unwrap_or(0)).collect();
        let hl_cols: Vec<bool> = cols.iter().map(|c| prefs.highlight.contains(&db::strip_indices(c))).collect();
        let related = if agg { HashMap::new() } else { self.t().map(|t| t.related.clone()).unwrap_or_default() };
        let in_coll = self.in_coll();
        let menu = self.row_menu_kind(agg);
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(1).show(ui, |ui| {
            let mut area = egui::ScrollArea::both().id_salt(("res", agg)).auto_shrink(false);
            // QUARRY_SCROLL_TEST slides the table sideways every frame, to measure scrolling cost.
            if std::env::var("QUARRY_SCROLL_TEST").is_ok() {
                area = area.horizontal_scroll_offset((ui.ctx().cumulative_frame_nr() % 60) as f32 * 25.0);
            }
            area.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let row_w = total.max(view_w - 2.0);
                // Header
                let (hr, _) = ui.allocate_exact_size(egui::vec2(row_w, TABLE_HEAD_H), Sense::hover());
                let p = ui.painter();
                p.rect_filled(hr, egui::CornerRadius { nw: R_MD - 1, ne: R_MD - 1, sw: 0, se: 0 }, HDR());
                p.text(egui::pos2(hr.left() + (num_w - NUM_W) + 18.0, hr.center().y), egui::Align2::LEFT_CENTER, "#", FontId::monospace(TXT_CAPTION), FAINT());
                if !agg {
                    let all = checked.len() == docs.len();
                    let r = check_box(ui, egui::pos2(hr.left() + 13.0, hr.center().y), ui.scope_id().with("check-all"), all, !all && !checked.is_empty());
                    if r.on_hover_text("Select every document on this page").clicked() {
                        check_all = Some(!all);
                    }
                }
                let mut x = hr.left() + num_w;
                for (ci, (c, w)) in cols.iter().zip(&widths).enumerate() {
                    let cell = egui::Rect::from_min_size(egui::pos2(x, hr.top()), egui::vec2(*w, hr.height()));
                    let pinned = prefs.pinned.contains(c);
                    let dir = dir_of(c);
                    // Inset so the resize handles at both edges stay on top for their clicks.
                    let hresp = ui.interact(cell.shrink2(egui::vec2(4.0, 0.0)), ui.scope_id().with(("h", c)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                    let state = state_of(&hresp);
                    ui.painter().rect_filled(cell, 0, hover_fill(state));
                    // Sorted in the accent with its direction, pinned in amber, fields missing from
                    // this page faint; hover brightens the rest.
                    let color = if dir.is_some() { PRIMARY() } else if pinned { WARNING() } else if state > 0 { BRIGHT() } else if fields.contains(c) { MUTED() } else { FAINT() };
                    let label = prefs.label(c);
                    let shown = match dir {
                        Some(d) if d > 0 => format!("{label} ↑"),
                        Some(_) => format!("{label} ↓"),
                        None => label.to_string(),
                    };
                    let g = ui.fonts_mut(|f| f.layout_job(LayoutJob::single_section(shown, ui_fmt(TXT_CAPTION, color, W_SEMIBOLD, 0.2))));
                    let ix = &indexed[ci];
                    let bolt = if ix.is_some() { BOLT_ROOM } else { 0.0 };
                    let gx = align_x(aligns[ci], x, *w - bolt, g.size().x);
                    let clip = ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0)));
                    if ix.is_some() {
                        paint_bolt(&clip, egui::pos2(gx + g.size().x + 4.0, cell.center().y));
                    }
                    clip.galley(egui::pos2(gx, cell.center().y - g.size().y / 2.0), g, color);
                    // Built only while hovered, not for every header on every frame.
                    let hresp = if label != c || ix.is_some() {
                        hresp.on_hover_ui(|ui| {
                            if label != c {
                                ui.label(RichText::new(c).monospace());
                            }
                            if let Some(ix) = ix {
                                ui.label(format!("Indexed: {ix}"));
                            }
                            ui.label("Click for column options");
                        })
                    } else {
                        hresp.on_hover_text("Click for column options")
                    };
                    // Left and right click both open it; a second left click closes it.
                    let open = if hresp.secondary_clicked() { Some(egui::SetOpenCommand::Bool(true)) } else if hresp.clicked() { Some(egui::SetOpenCommand::Toggle) } else { None };
                    let mut acts = Vec::new();
                    // Clicks inside, such as the Data scope switch, keep it open; each action closes it itself.
                    egui::Popup::menu(&hresp).open_memory(open).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(190.0).show(|ui| col_popover(ui, c, agg, dir, &prefs, &mut scope_query, &mut acts));
                    head_acts.extend(acts.into_iter().map(|a| (c.clone(), a)));
                    // Drag the right edge to resize.
                    let handle = egui::Rect::from_min_max(egui::pos2(cell.right() - 3.0, cell.top() + 6.0), egui::pos2(cell.right() + 3.0, cell.bottom() - 6.0));
                    let dr = ui.interact(handle, ui.scope_id().with(("w", c)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeColumn).on_hover_text("Drag to resize, double-click to fit");
                    if dr.double_clicked() {
                        fit = Some(c.clone());
                    }
                    ui.painter().vline(cell.right(), handle.y_range(), Stroke::new(if dr.hovered() || dr.dragged() { 2.0 } else { 1.0 }, if dr.hovered() || dr.dragged() { PRIMARY() } else { LINE() }));
                    if dr.dragged() {
                        resized = Some((c.clone(), (*w + dr.drag_delta().x).clamp(COL_MIN_W, COL_MAX_W)));
                    }
                    if dr.drag_stopped() {
                        save_widths = true;
                    }
                    x += w;
                }
                ui.painter().hline(hr.x_range(), hr.bottom() - 0.5, Stroke::new(1.0, LINE()));
                // Laid out once for every empty cell of the page.
                let dash = ui.fonts_mut(|f| f.layout_no_wrap("—".into(), FontId::monospace(TXT_SMALL), FAINT()));
                // Rows (at most one page, so no virtualisation is needed)
                for i in 0..docs.len() {
                    let drafted = self.shown_row(docs, i, agg);
                    let value = |c: &str| match &drafted {
                        Some(d) => db::get_path(d, c).map(Val::B),
                        None => docs.value(i, c),
                    };
                    let active = selected == Some((agg, i));
                    let (rr, resp) = ui.allocate_exact_size(egui::vec2(row_w, ROW_H), Sense::click());
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if active && std::mem::take(&mut self.scroll_sel) {
                        resp.scroll_to_me(None);
                    }
                    // Rows scrolled out of view keep their space but skip all layout and painting.
                    if ui.is_rect_visible(rr) {
                        let hovered = ui.rect_contains_pointer(rr) && !ui.ctx().any_popup_open();
                        let fill = row_fill(active, checked.contains(&i), hovered, resp.is_pointer_button_down_on(), i % 2 == 1);
                        let p = ui.painter();
                        if fill != Color32::TRANSPARENT {
                            p.rect_filled(rr, 0, fill);
                        }
                        let off = num_w - NUM_W;
                        if self.prefs.open_inline {
                            chevron(p, egui::pos2(rr.left() + off + 9.0, rr.center().y), active || expanded.contains(&(agg, i)), if active { PRIMARY() } else { FAINT() });
                        }
                        let num = (base + i as u64 + 1).to_string();
                        let (num, nc) = if drafted.is_some() { (format!("{num}●"), WARNING()) } else { (num, FAINT()) };
                        p.text(egui::pos2(rr.left() + off + 18.0, rr.center().y), egui::Align2::LEFT_CENTER, num, FontId::monospace(TXT_CAPTION), nc);
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
                            let v = value(c);
                            match v {
                                Some(v) => {
                                    let mut job = column_job(v, summarize);
                                    // Ends in an ellipsis at the cell's padding instead of being cut mid-glyph by the clip.
                                    job.wrap = egui::text::TextWrapping::truncate_at_width(w - 14.0);
                                    let g = ui.fonts_mut(|f| f.layout_job(job));
                                    let gw = g.size().x;
                                    // Ids only, checked before compact(): that turns the whole value into JSON,
                                    // which is costly for big objects on every frame.
                                    let rel = (!related.is_empty() && v.id_like()).then(|| related.get(&db::compact(&v.bson()))).flatten();
                                    let rg = rel.map(|p| ui.fonts_mut(|f| f.layout_no_wrap(p.clone(), FontId::proportional(TXT_SMALL), C_LINK())));
                                    // The value and its preview are aligned as one, so the preview stays inside the cell.
                                    let vx = align_x(aligns[ci], x, *w, gw + rg.as_ref().map_or(0.0, |r| 6.0 + r.size().x));
                                    clip.galley(egui::pos2(vx, cell.center().y - g.size().y / 2.0), g, TEXT());
                                    if let Some(r) = rg {
                                        clip.galley(egui::pos2(vx + gw + 6.0, cell.center().y - r.size().y / 2.0), r, C_LINK());
                                    }
                                }
                                None => {
                                    let g = dash.clone();
                                    clip.galley(egui::pos2(align_x(aligns[ci], x, *w, g.size().x), cell.center().y - g.size().y / 2.0), g, FAINT());
                                }
                            }
                            let cr = ui.interact(cell, ui.scope_id().with(("c", i, ci)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                            if cr.double_clicked() {
                                edit_cell = Some((i, c.clone()));
                            } else if cr.clicked() {
                                click = Some(i);
                            }
                            let mut acts = Vec::new();
                            if let Some(v) = v {
                                let path: Vec<String> = c.split('.').map(String::from).collect();
                                cr.context_menu(|ui| {
                                    menu_look(ui);
                                    // Field and document actions together run about 800 pt, taller than many windows.
                                    menu_scroll(ui, |ui| {
                                        field_menu(ui, &path, &v.bson(), !agg, in_coll, hl_cols[ci], &mut acts);
                                        if menu != RowMenu::None {
                                            ui.separator();
                                            row_act = any_row_menu(ui, menu, i, docs, &checked).or(row_act.take());
                                        }
                                    });
                                });
                            } else if menu != RowMenu::None {
                                cr.context_menu(|ui| {
                                    menu_look(ui);
                                    if !agg {
                                        if ui.button("Edit full document as JSON…").clicked() {
                                            acts.push(TreeAct::EditDoc);
                                        }
                                        ui.separator();
                                    }
                                    row_act = any_row_menu(ui, menu, i, docs, &checked).or(row_act.take());
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
                    if menu != RowMenu::None {
                        resp.context_menu(|ui| row_act = any_row_menu(ui, menu, i, docs, &checked).or(row_act.take()));
                    }
                    if (active || expanded.contains(&(agg, i))) && self.prefs.open_inline {
                        ui.add_space(4.0);
                        // Spans every column like the rows above it, not just the visible part.
                        let w = row_w.max(320.0);
                        card(ui, |ui| {
                            // Card inner margin and stroke on both sides.
                            ui.set_width(w - 30.0);
                            ui.spacing_mut().item_spacing.y = 6.0;
                            if let Some(d) = drafted.as_ref().or_else(|| docs.doc(i)) {
                                self.doc_body(ui, d, agg, Some((agg, i)));
                            }
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
        self.col_scope_query = scope_query;
        for (c, a) in head_acts {
            self.head_act(ui, agg, docs, fields, &c, a);
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
        self.row_act(ui, row_act, docs, &checked);
        for (i, acts) in cell_acts {
            self.select(agg, i, false);
            let Some(d) = self.shown_row(docs, i, agg).or_else(|| docs.owned(i)) else { continue };
            self.handle_acts(&d, acts);
        }
        if let (Some((i, c)), false) = (edit_cell, agg) {
            self.select(agg, i, false);
            let path: Vec<String> = c.split('.').map(String::from).collect();
            let d = self.shown_row(docs, i, false).or_else(|| docs.owned(i)).unwrap_or_default();
            match db::get_path(&d, &c).cloned() {
                Some(v) if c != "_id" => self.start_inline(&d, path, v),
                Some(_) => {}
                None => self.err(format!("This document has no field {c}. Use + Add field.")),
            }
        }
    }

    pub(super) fn doc_list(&mut self, ui: &mut egui::Ui, agg: bool, docs: Rows) {
        let selected = self.t().unwrap().selected;
        let checked = if agg { BTreeSet::new() } else { self.t().unwrap().checked.clone() };
        let expanded = self.t().unwrap().expanded.clone();
        let mut row_act = None;
        let menu = self.row_menu_kind(agg);
        let view_w = ui.available_width();
        let mut click = None;
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(1).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("list", agg)).auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in 0..docs.len() {
                    let drafted = self.shown_row(docs, i, agg);
                    let active = selected == Some((agg, i));
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if active && std::mem::take(&mut self.scroll_sel) {
                        resp.scroll_to_me(None);
                    }
                    let fill = row_fill(active, checked.contains(&i), resp.hovered(), resp.is_pointer_button_down_on(), i % 2 == 1);
                    ui.painter().rect_filled(rect, 0, fill);
                    let job = match &drafted {
                        Some(d) => row_job(DocRef::B(d), w - 16.0),
                        None => row_job(docs.doc_ref(i), w - 16.0),
                    };
                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    ui.painter().galley(rect.left_center() + egui::vec2(8.0, -galley.size().y / 2.0), galley, TEXT());
                    if resp.clicked() {
                        click = Some(i);
                    }
                    if menu != RowMenu::None {
                        resp.context_menu(|ui| row_act = any_row_menu(ui, menu, i, docs, &checked).or(row_act.take()));
                    }
                    if (active || expanded.contains(&(agg, i))) && self.prefs.open_inline {
                        ui.add_space(4.0);
                        card(ui, |ui| {
                            ui.set_width(view_w - 48.0);
                            ui.spacing_mut().item_spacing.y = 6.0;
                            if let Some(d) = drafted.as_ref().or_else(|| docs.doc(i)) {
                                self.doc_body(ui, d, agg, Some((agg, i)));
                            }
                        });
                        ui.add_space(6.0);
                    }
                }
            });
        });
        if let Some(i) = click {
            self.row_click(ui, agg, i);
        }
        self.row_act(ui, row_act, docs, &checked);
    }

    /// Documents rows get the document menu, Indexes rows the index menu, other result rows none.
    fn row_menu_kind(&self, agg: bool) -> RowMenu {
        match (agg, self.t().map(|t| t.view)) {
            (false, _) => RowMenu::Docs,
            (true, Some(View::Indexes)) => RowMenu::Indexes,
            _ => RowMenu::None,
        }
    }

    fn row_act(&mut self, ui: &egui::Ui, act: Option<(usize, RowAct)>, docs: Rows, checked: &BTreeSet<usize>) {
        match act {
            Some((i, RowAct::Doc(a))) => self.docs_act(&ui.ctx().clone(), a, Self::act_targets(checked, docs, i)),
            Some((i, RowAct::Idx(a))) => {
                if let Some(d) = docs.doc(i) {
                    self.index_act(ui.ctx(), d, a);
                }
            }
            None => {}
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

/// A result row's fill: selection, then checked, pressed and hovered, then the zebra stripe.
fn row_fill(active: bool, checked: bool, hovered: bool, pressed: bool, odd: bool) -> Color32 {
    match () {
        _ if active => if hovered { SEL_HOVER() } else { SEL() },
        _ if checked => tint(PRIMARY(), if hovered { 38 } else { 26 }),
        _ if pressed => PRESSED(),
        _ if hovered => HOVER(),
        _ if odd => pal().stripe,
        _ => Color32::TRANSPARENT,
    }
}
