//! The results table and list: columns, widths and painting rows.

use super::*;

/// SQL cell work found while the grid was drawn, done by `results` once the page is back in the tab.
#[derive(Default)]
pub(super) struct SqlAfter {
    drill: Option<usize>,
    edit: Option<(usize, String)>,
    finish: bool,
    acts: Vec<(usize, String, CellAct)>,
    fk: Option<(db::sql::structure::ForeignKey, Document)>,
}

impl App {
    /// In the order the grid used to do it: drill into a group, open, finish, the cell menu, then a link click, which stages the edit its
    /// click finished before it moves to the other row.
    pub(super) fn sql_after(&mut self, ctx: &egui::Context, a: SqlAfter) {
        if let Some(i) = a.drill {
            self.sql_drill(i);
        }
        if let Some((i, c)) = a.edit {
            self.sql_open_cell(i, c);
        }
        if a.finish {
            self.sql_cell_finish();
            self.sql_cell_acts(ctx, a.acts);
        }
        if let Some((fk, d)) = a.fk {
            self.open_fk_row(&fk, &d);
        }
    }
}

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

    /// The top-level value `key` of row `i`, for rows whose keys may hold a dot.
    pub(super) fn key(self, i: usize, key: &str) -> Option<Bson> {
        match self {
            Rows::Docs(d) => d.get(i)?.get(key).cloned(),
            Rows::Raw(r) => Bson::try_from(r.get(i)?.raw().get(key).ok()??).ok(),
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

/// Width kept at the right of a foreign key cell for its link.
const FK_W: f32 = 22.0;

/// The small link at the right edge of a foreign key cell: a box with an arrow leaving it, painted as shapes.
fn fk_link(ui: &mut egui::Ui, cell: egui::Rect, id: egui::Id, fk: &db::sql::structure::ForeignKey) -> egui::Response {
    let area = egui::Rect::from_min_max(egui::pos2(cell.right() - FK_W, cell.top()), cell.right_bottom());
    let r = ui.interact(area, id, Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("Open the row in {}", fk.ref_table));
    let btn = egui::Rect::from_center_size(area.center(), egui::vec2(16.0, 16.0));
    if r.is_pointer_button_down_on() {
        ui.painter().rect_filled(btn, R_SM, PRESSED());
    } else if r.hovered() {
        ui.painter().rect_filled(btn, R_SM, HOVER());
    }
    let color = if r.hovered() { C_LINK() } else { tint(C_LINK(), 170) };
    let stroke = Stroke::new(1.4, color);
    let c = btn.center();
    let (a, b) = (c + egui::vec2(-3.0, 3.0), c + egui::vec2(3.0, -3.0));
    let p = ui.painter();
    p.line_segment([a, b], stroke);
    p.line_segment([b + egui::vec2(-3.5, 0.0), b], stroke);
    p.line_segment([b + egui::vec2(0.0, 3.5), b], stroke);
    r
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

/// How many columns show before the user picks some. SQL tables are narrow enough to show whole,
/// while Mongo documents can have hundreds of fields.
fn default_n(sql: bool, setting: usize) -> usize {
    if sql { usize::MAX } else { setting }
}

impl App {
    pub(super) fn default_cols(&self) -> usize {
        default_n(self.t().is_some_and(|t| t.sql.is_some()), self.prefs.default_cols)
    }

    pub(super) fn columns_menu(&mut self, ui: &mut egui::Ui, agg: bool) {
        let key = self.col_key(agg);
        let t = self.t().unwrap();
        let fields = if agg { t.agg_source().1.to_vec() } else { t.fields.clone() };
        let prefs = self.prefs.cols.get(&key).cloned().unwrap_or_default();
        let n = self.default_cols();
        let shown = visible_cols(Some(&prefs), &fields, n);
        let mut acts = Vec::new();
        let mut add = None;
        sticky_menu(ui, format!("Columns {}", shown.len()), |ui| {
            popover_body(ui, 360.0, |ui| {
                let total = shown.len() + fields.iter().filter(|f| !shown.contains(f)).count();
                popover_head(ui, "Columns", &format!("{} of {total}", shown.len()), "Drag to reorder. Pinned columns stay first.");
                // One field filters the lists and adds a path the lists do not offer. Its suggestions are
                // nested paths seen in the loaded documents.
                let paths: &[String] = if agg { &fields } else { &self.tabs[self.cur].paths };
                let suggest = |b: &str, _: bool| {
                    let typed = b.trim().to_lowercase();
                    let items: Vec<Sug> = paths
                        .iter()
                        .filter(|p| !typed.is_empty() && p.to_lowercase().contains(&typed) && !shown.contains(p))
                        .take(50)
                        .map(|p| Sug { sql: None, back: 0, text: p.clone(), note: "field", insert: p.clone(), caret: p.chars().count() })
                        .collect();
                    (!items.is_empty()).then(|| (b.chars().count(), items))
                };
                let id = ui.make_persistent_id("custom-col");
                let mut moved = false;
                let (mut enter, mut picked_path) = (false, false);
                well_field(ui, id, FieldIcon::Search, None, |ui| {
                    let w = ui.available_width();
                    let (out, picked, m) = ac_input(ui, id, &mut self.col_filter, &mut self.ac, &suggest, |ui, text, _| field_edit(text, id, "Filter fields, or type a path to add", w, egui::TextStyle::Monospace).show(ui));
                    moved = m;
                    picked_path = picked;
                    enter = picked || (out.response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
                });
                // Drawn in the menu, not as a popup: a click on another layer would close the menu.
                if ac_list(ui, id, &mut self.col_filter, &mut self.ac, moved, Some(ui.available_width() - 2.0)) {
                    (enter, picked_path) = (true, true);
                }
                ui.add_space(12.0);
                let typed = self.col_filter.trim().to_string();
                let needle = typed.to_lowercase();
                let keep = |c: &str| needle.is_empty() || c.to_lowercase().contains(&needle);
                let hidden: Vec<&String> = fields.iter().filter(|f| !shown.contains(f) && keep(f)).collect();
                let shown_kept: Vec<(usize, &String)> = shown.iter().enumerate().filter(|(_, c)| keep(c)).collect();
                // Enter adds a picked suggestion, the one field a filter leaves, or a path nothing matches.
                // A filter that only narrows the lists adds nothing.
                if enter && !typed.is_empty() {
                    if picked_path {
                        add = Some(typed.clone());
                    } else if hidden.len() == 1 {
                        add = Some(hidden[0].clone());
                    } else if hidden.is_empty() && shown_kept.is_empty() {
                        add = Some(typed.clone());
                    }
                }
                // Always drawn, so Reset is there even when every column is hidden or filtered out.
                if section_head(ui, "SHOWN", Some("Reset")).is_some_and(|r| r.on_hover_text("Show the default columns again").clicked()) {
                    acts.push(ColAct::Reset);
                }
                if !shown_kept.is_empty() {
                    let last_pin = shown.iter().rposition(|c| prefs.pinned.contains(c));
                    egui::ScrollArea::vertical().id_salt("shown").max_height(320.0).show(ui, |ui| {
                        for &(i, c) in &shown_kept {
                            let (row, hovered) = pop_row(ui, POP_ROW_H);
                            let rect = row.rect;
                            let grip_rect = egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 2.0), egui::vec2(14.0, 26.0));
                            put_at(ui, grip_rect, |ui| ui.dnd_drag_source(egui::Id::unique(("col-drag", c)), i, grip).response.on_hover_cursor(egui::CursorIcon::Grab));
                            let p = ui.painter().clone();
                            mono_at(&p, rect.left() + 22.0, rect.center().y, c, BRIGHT());
                            let pinned = prefs.pinned.contains(c);
                            let mut right = rect.right();
                            if hovered {
                                let at = egui::Rect::from_min_size(egui::pos2(right - BUTTON_H, rect.center().y - BUTTON_H / 2.0), egui::vec2(BUTTON_H, BUTTON_H));
                                if put_at(ui, at, eye_button).on_hover_text("Hide").clicked() {
                                    acts.push(ColAct::Hide(c.clone()));
                                }
                                right -= BUTTON_H + 2.0;
                            }
                            if hovered || pinned {
                                let at = egui::Rect::from_min_size(egui::pos2(right - BUTTON_H, rect.center().y - BUTTON_H / 2.0), egui::vec2(BUTTON_H, BUTTON_H));
                                if put_at(ui, at, |ui| pin_button(ui, pinned)).on_hover_text(if pinned { "Unpin" } else { "Pin: keep this column first" }).clicked() {
                                    acts.push(ColAct::Pin(c.clone()));
                                }
                                right -= BUTTON_H + 6.0;
                            }
                            if !fields.contains(c) {
                                tag_at(&p, right, rect.center().y, "path", MUTED());
                            }
                            // Pinned columns sit above a dashed line.
                            if last_pin == Some(i) && i + 1 < shown.len() {
                                let y = rect.bottom();
                                p.extend(egui::Shape::dashed_line(&[egui::pos2(rect.left() + 22.0, y), egui::pos2(rect.right(), y)], Stroke::new(1.0, RAISED_LINE()), 3.0, 3.0));
                            }
                            // Drop on the upper half to go before this row, the lower half to go after it.
                            let below = ui.input(|inp| inp.pointer.hover_pos()).is_some_and(|p| p.y > rect.center().y);
                            if row.dnd_hover_payload::<usize>().is_some() {
                                let y = if below { rect.bottom() } else { rect.top() };
                                ui.painter().hline(rect.x_range(), y, Stroke::new(2.0, PRIMARY()));
                            }
                            if let Some(from) = row.dnd_release_payload::<usize>() {
                                acts.push(ColAct::MoveTo(shown[*from].clone(), i + below as usize));
                            }
                        }
                    });
                }
                if !hidden.is_empty() {
                    ui.add_space(8.0);
                    section_head(ui, &format!("MORE FIELDS {}", hidden.len()), None);
                    egui::ScrollArea::vertical().id_salt("more").max_height(200.0).show(ui, |ui| {
                        for f in hidden {
                            let (row, hovered) = pop_row(ui, POP_ROW_H);
                            let (rect, p) = (row.rect, ui.painter());
                            let color = if hovered { BRIGHT() } else { MUTED() };
                            paint_plus(p, egui::pos2(rect.left() + 6.0, rect.center().y), 4.0, color);
                            mono_at(p, rect.left() + 22.0, rect.center().y, f, color);
                            if hovered {
                                let g = p.layout_no_wrap("Show".into(), FontId::proportional(TXT_CAPTION), MUTED());
                                galley_at(p, rect.right() - g.size().x, rect.center().y, g, MUTED());
                            }
                            if row.clicked() {
                                acts.push(ColAct::Show(f.clone()));
                            }
                        }
                    });
                }
                if shown_kept.is_empty() && !typed.is_empty() && !fields.iter().any(|f| keep(f)) {
                    empty_state(ui, paint_search, "No field matches", "Not in the loaded documents. Paths still work.", |ui| {
                        ui.add_space(10.0);
                        if add_chip(ui, &format!("Add {typed} as a column")).clicked() {
                            add = Some(typed.clone());
                        }
                    });
                }
                popover_foot(ui, None, &["Dotted paths reach inside objects and arrays, like items.0.sku."]);
            });
        });
        if let Some(c) = add {
            acts.push(ColAct::Show(c));
            self.col_filter.clear();
        }
        if !acts.is_empty() {
            self.apply_col_acts(&key, &fields, acts);
        }
    }

    pub(super) fn apply_col_acts(&mut self, key: &str, fields: &[String], acts: Vec<ColAct>) {
        let n = self.default_cols();
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

    /// Draws the rows. SQL cell work found on the way comes back in `SqlAfter`, because the page is out of the tab
    /// while it is drawn (see `results`) and that work looks rows up in the tab.
    pub(super) fn doc_table(&mut self, ui: &mut egui::Ui, agg: bool, docs: Rows, fields: &[String], base: u64) -> SqlAfter {
        let key = self.col_key(agg);
        let prefs = self.prefs.cols.get(&key).cloned().unwrap_or_default();
        let cols = visible_cols(Some(&prefs), fields, self.default_cols());
        let indexed: Vec<Option<String>> = cols.iter().map(|c| self.t().and_then(|t| t.indexes_on(c)).map(str::to_string)).collect();
        let summarize = !self.prefs.object_preview;
        // Measuring lays out every loaded cell of the column, so it is done once per result set instead of
        // on every frame. The generation says which answer the rows came from; where they live and how
        // many say which window of it is on screen.
        let answer = self.t().map_or(0, |t| t.rows_gen);
        let widths: Vec<f32> = cols
            .iter()
            .map(|c| {
                prefs.widths.get(c).copied().unwrap_or_else(|| {
                    // The header text is measured too, so a custom label is part of the key.
                    let k = (answer, docs.addr(), docs.len(), c.clone(), prefs.labels.get(c).cloned(), summarize);
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
        // The Documents view and the Shell's result take checks; other results do not.
        let checks = !agg || self.t().is_some_and(|t| t.view == View::Shell);
        let num_w = if checks { NUM_W + CHECK_W } else { NUM_W };
        let total: f32 = num_w + widths.iter().sum::<f32>();
        let selected = self.t().unwrap().selected;
        let checked = if checks { self.checks().clone() } else { BTreeSet::new() };
        let expanded = self.t().unwrap().expanded.clone();
        let mut check_click = None;
        let mut check_all = None;
        let mut row_act = None;
        let view_w = ui.available_width();
        let mut click = None;
        let mut edit_cell = None;
        // A double click on a row of grouped results: open that group's rows.
        let mut drill = None;
        let mut resized = None;
        let mut save_widths = false;
        let mut fit = None;
        let mut head_acts: Vec<(String, HeadAct)> = Vec::new();
        let mut scope_query = self.col_scope_query;
        let mut cell_acts: Vec<(usize, Vec<TreeAct>)> = Vec::new();
        // Single-field sort shown in the header and set from the column popover.
        let sort = if agg { None } else { self.t().and_then(|t| t.query.sort.clone()).filter(|s| s.len() == 1) };
        // A SQL tab's sort is the ORDER BY text, which a header click writes as "`col` ASC".
        let sql_sort = self.t().filter(|t| t.sql.is_some()).map(|t| t.sort_txt.clone());
        let sql = sql_sort.is_some();
        let group_cols: Vec<String> = self.t().and_then(|t| t.sql.as_ref()).filter(|s| sql && s.matched.is_some()).map(|s| super::sql::bar::group_names(s)).unwrap_or_default();
        // A Shell result shows the same tab but is not the table's rows: its indexes and keys mean nothing to the editor.
        let sql_rows = sql && !agg;
        let sql_grouped = sql && self.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.matched.is_some());
        let sql_dialect = self.sql_dialect();
        let dir_of = |c: &str| match &sql_sort {
            Some(s) => {
                let q = sql_dialect.quote_ident(c);
                if *s == format!("{q} ASC") { Some(1) } else if *s == format!("{q} DESC") { Some(-1) } else { None }
            }
            None => sort.as_ref().and_then(|s| s.get(c)).and_then(|v| match v {
                Bson::Int32(n) => Some(*n as i64),
                Bson::Int64(n) => Some(*n),
                Bson::Double(f) => Some(*f as i64),
                _ => None,
            }),
        };
        let kinds: Vec<Option<db::sql::value::Kind>> = cols.iter().map(|c| self.t().and_then(|t| t.sql.as_ref()).and_then(|s| s.cols.iter().find(|k| &k.name == c)).map(|k| k.kind)).collect();
        // Numbers read best right-aligned, so SQL numeric columns start that way unless the user chose otherwise.
        let aligns: Vec<u8> = cols.iter().zip(&kinds).map(|(c, k)| prefs.align.get(c).copied().unwrap_or(if k.is_some_and(is_numeric) { 2 } else { 0 })).collect();
        // Why a SQL column cannot be edited, or that writes are still locked; shown when a cell is hovered.
        let locked = sql && !self.sel_writable() && !self.sel_is_view();
        let sql_why: Vec<Option<String>> = cols
            .iter()
            .map(|c| {
                if !sql_rows {
                    return None;
                }
                self.sql_edit_block().or_else(|| self.sql_col_block(c)).map(String::from).or_else(|| locked.then(|| "Read-only connection. Double-click to unlock writes and edit.".to_string()))
            })
            .collect();
        let sql_edit_ok: Vec<bool> = cols.iter().map(|c| sql_rows && self.sql_edit_block().is_none() && self.sql_col_block(c).is_none()).collect();
        let sql_nullable: Vec<bool> = cols.iter().map(|c| self.t().and_then(|t| t.sql.as_ref()).and_then(|s| s.cols.iter().find(|k| &k.name == c)).is_some_and(|k| k.nullable)).collect();
        let mut sql_acts: Vec<(usize, String, CellAct)> = Vec::new();
        // A column that belongs to a foreign key gets a link that opens the row it points to.
        let fks = if sql && !agg && !sql_grouped { self.sql_fks() } else { Vec::new() };
        let fk_of: Vec<Option<usize>> = cols.iter().map(|c| self.t().and_then(|t| t.sql.as_ref()).and_then(|s| s.cols.iter().find(|k| &k.name == c)).and_then(|k| fks.iter().position(|f| f.cols.contains(&k.src)))).collect();
        let mut fk_open: Option<(usize, usize)> = None;
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
                if checks {
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
                    hresp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, c.as_str()));
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
                    let (gw, gh) = (g.size().x, g.size().y);
                    clip.galley(egui::pos2(gx, cell.center().y - gh / 2.0), g, color);
                    if group_cols.contains(c) {
                        let tag = ui.fonts_mut(|f| f.layout_no_wrap("group".into(), FontId::monospace(TXT_CAPTION - 1.0), FAINT()));
                        clip.galley(egui::pos2(gx + gw + 6.0, cell.center().y - tag.size().y / 2.0), tag, FAINT());
                    }
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
                    // Shift-click on a SQL header adds a sort key instead of opening the menu.
                    let shift_sort = sql && hresp.clicked() && ui.input(|i| i.modifiers.shift);
                    if shift_sort {
                        head_acts.push((c.clone(), HeadAct::AddSort));
                    }
                    let open = if hresp.secondary_clicked() { Some(egui::SetOpenCommand::Bool(true)) } else if hresp.clicked() && !shift_sort { Some(egui::SetOpenCommand::Toggle) } else { None };
                    let mut acts = Vec::new();
                    // Clicks inside, such as the Data scope switch, keep it open; each action closes it itself.
                    egui::Popup::menu(&hresp).open_memory(open).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(190.0).show(|ui| col_popover(ui, c, agg, sql, dir, &prefs, &mut scope_query, &mut acts));
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
                    let staged = if sql && drafted.is_some() { self.sql_staged_cols(docs, i) } else { Vec::new() };
                    let active = selected == Some((agg, i));
                    let (rr, resp) = ui.allocate_exact_size(egui::vec2(row_w, ROW_H), Sense::click());
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if active && std::mem::take(&mut self.scroll_sel) {
                        resp.scroll_to_me(None);
                    }
                    // Rows scrolled out of view keep their space but skip all layout and painting.
                    if ui.is_rect_visible(rr) {
                        let hovered = ui.rect_contains_pointer(rr) && !ui.ctx().any_popup_open();
                        paint_row(ui, rr, active, checked.contains(&i), hovered, resp.is_pointer_button_down_on(), i % 2 == 1);
                        let p = ui.painter();
                        let off = num_w - NUM_W;
                        if self.prefs.open_inline && !sql {
                            chevron(p, egui::pos2(rr.left() + off + 9.0, rr.center().y), active || expanded.contains(&(agg, i)), if active { PRIMARY() } else { FAINT() });
                        }
                        let num = (base + i as u64 + 1).to_string();
                        let (num, nc) = if drafted.is_some() { (format!("{num}●"), WARNING()) } else { (num, FAINT()) };
                        p.text(egui::pos2(rr.left() + off + 18.0, rr.center().y), egui::Align2::LEFT_CENTER, num, FontId::monospace(TXT_CAPTION), nc);
                        if checks {
                            let cb = check_box(ui, egui::pos2(rr.left() + 13.0, rr.center().y), ui.scope_id().with(("check", i)), checked.contains(&i), false);
                            cb.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, checked.contains(&i), format!("Select row {}", base + i as u64 + 1)));
                            if cb.clicked() {
                                check_click = Some(i);
                            }
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
                            // An unsaved edit: the warning tint with a bar on the left edge.
                            if staged.iter().any(|s| s == c) {
                                ui.painter().rect_filled(cell, 0, tint(WARNING(), 34));
                                ui.painter().rect_filled(egui::Rect::from_min_size(cell.min, egui::vec2(2.0, cell.height())), 0, WARNING());
                            }
                            if sql_rows && self.sql_cell.as_ref().is_some_and(|e| e.is_at(docs.id(i), c)) {
                                self.sql_cell_editor(ui, cell);
                                x += w;
                                continue;
                            }
                            let clip = ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0)));
                            let v = value(c);
                            match v {
                                Some(v) => {
                                    let mut job = match kinds[ci] {
                                        Some(k) => sql_cell_job(v, k, true),
                                        None => column_job(v, summarize),
                                    };
                                    // Ends in an ellipsis at the cell's padding instead of being cut mid-glyph by the clip.
                                    let fk_room = if fk_of[ci].is_some() { FK_W } else { 0.0 };
                                    job.wrap = egui::text::TextWrapping::truncate_at_width(w - 14.0 - fk_room);
                                    let g = ui.fonts_mut(|f| f.layout_job(job));
                                    let gw = g.size().x;
                                    // Ids only, checked before compact(): that turns the whole value into JSON,
                                    // which is costly for big objects on every frame.
                                    let rel = (!related.is_empty() && v.id_like()).then(|| related.get(&db::compact(&v.bson()))).flatten();
                                    let rg = rel.map(|p| ui.fonts_mut(|f| f.layout_no_wrap(p.clone(), FontId::proportional(TXT_SMALL), C_LINK())));
                                    // The value and its preview are aligned as one, so the preview stays inside the cell.
                                    let vx = align_x(aligns[ci], x, *w - fk_room, gw + rg.as_ref().map_or(0.0, |r| 6.0 + r.size().x));
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
                            let mut cr = ui.interact(cell, ui.scope_id().with(("c", i, ci)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                            // Named after the row number the grid shows, with the cell text as its value, so tests find it.
                            cr.widget_info(|| {
                                let mut info = egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{c} row {}", base + i as u64 + 1));
                                info.current_text_value = v.map(|v| cell_text(&v.bson()));
                                info
                            });
                            if let Some(why) = &sql_why[ci] {
                                cr = cr.on_hover_text(why.as_str());
                            }
                            // Added after the cell so it takes the click; a NULL points to nothing and gets no link.
                            if let (Some(k), Some(val)) = (fk_of[ci], v) && !matches!(*val.bson(), Bson::Null) {
                                let fr = fk_link(ui, cell, ui.scope_id().with(("fk", i, ci)), &fks[k]);
                                if fr.clicked() {
                                    fk_open = Some((i, k));
                                }
                            }
                            if cr.double_clicked() && sql_grouped {
                                drill = Some(i);
                            } else if cr.double_clicked() {
                                edit_cell = Some((i, c.clone()));
                            } else if cr.clicked() {
                                click = Some(i);
                            }
                            let mut acts = Vec::new();
                            if sql && !agg {
                                cr.context_menu(|ui| {
                                    menu_look(ui);
                                    // A checked row's menu is the selection's; a single cell also offers its value actions.
                                    if !checked.contains(&i) {
                                        if let Some(a) = sql_cell_menu(ui, sql_nullable[ci], sql_edit_ok[ci]) {
                                            sql_acts.push((i, c.clone(), a));
                                            ui.close();
                                        }
                                        ui.separator();
                                    }
                                    row_act = any_row_menu(ui, menu, i, docs, &checked).or(row_act.take());
                                });
                            } else if let (Some(v), false) = (v, sql) {
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
                    if (active || expanded.contains(&(agg, i))) && self.prefs.open_inline && !sql {
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
            *self.checks_mut().0 = if on { (0..docs.len()).collect() } else { BTreeSet::new() };
        }
        self.row_act(ui, row_act, docs, &checked);
        for (i, acts) in cell_acts {
            self.select(agg, i, false);
            let Some(d) = self.shown_row(docs, i, agg).or_else(|| docs.owned(i)) else { continue };
            self.handle_acts(&d, acts);
        }
        let after = SqlAfter {
            drill,
            edit: edit_cell.clone().filter(|_| sql_rows),
            finish: sql,
            acts: sql_acts,
            fk: fk_open.and_then(|(i, k)| self.shown_row(docs, i, agg).or_else(|| docs.owned(i)).map(|d| (fks[k].clone(), d))),
        };
        if let (Some((i, c)), false) = (edit_cell, agg || sql) {
            self.select(agg, i, false);
            let path: Vec<String> = c.split('.').map(String::from).collect();
            let d = self.shown_row(docs, i, false).or_else(|| docs.owned(i)).unwrap_or_default();
            match db::get_path(&d, &c).cloned() {
                Some(v) if c != "_id" => self.start_inline(&d, path, v),
                Some(_) => {}
                None => self.warn(format!("This document has no field {c}. Use + Add field.")),
            }
        }
        after
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
                    paint_row(ui, rect, active, checked.contains(&i), resp.hovered(), resp.is_pointer_button_down_on(), i % 2 == 1);
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
            (false, _) if self.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.matched.is_some()) => RowMenu::SqlGrouped,
            (false, _) if self.t().is_some_and(|t| t.sql.is_some()) => RowMenu::Sql,
            (false, _) => RowMenu::Docs,
            (true, Some(View::Indexes)) => RowMenu::Indexes,
            (true, Some(View::Shell)) => RowMenu::Results(self.t().is_some_and(|t| t.sql.is_some())),
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

/// A result row's fill: selection, then checked, pressed and hovered, then the zebra stripe.
pub(super) fn row_fill(active: bool, checked: bool, hovered: bool, pressed: bool, odd: bool) -> Color32 {
    match () {
        _ if active => if hovered { SEL_HOVER() } else { SEL() },
        _ if checked => tint(PRIMARY(), if hovered { 38 } else { 26 }),
        // Neutral so it never looks like the accent selection; a press goes one shade further.
        _ if pressed && reading_line() => shade(PRESSED(), 0.12),
        _ if pressed => PRESSED(),
        _ if hovered && reading_line() => PRESSED(),
        _ if hovered => HOVER(),
        _ if odd => pal().stripe,
        _ => Color32::TRANSPARENT,
    }
}

/// The reading line's bar sits at the visible left edge, so it stays in view when scrolled sideways.
pub(super) fn paint_row(ui: &egui::Ui, row: egui::Rect, active: bool, checked: bool, hovered: bool, pressed: bool, odd: bool) {
    let fill = row_fill(active, checked, hovered, pressed, odd);
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(row, 0, fill);
    }
    if hovered && !active && reading_line() {
        let x = row.left().max(ui.clip_rect().left());
        ui.painter().rect_filled(egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(3.0, row.height())), 0, PRIMARY());
    }
}

/// The hide button of a Columns row: an eye with a slash.
fn eye_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(BUTTON_H, BUTTON_H), Sense::click());
    let state = state_of(&r);
    ui.painter().rect_filled(rect, R_SM, hover_fill_on(ui, state));
    paint_eye_off(ui.painter(), rect.center(), if state > 0 { BRIGHT() } else { MUTED() });
    focus_ring(ui, &r, rect, R_SM as f32);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_shows_every_column_by_default() {
        let fields: Vec<String> = (0..30).map(|i| format!("f{i}")).collect();
        assert_eq!(visible_cols(None, &fields, default_n(true, DEFAULT_COLS)).len(), 30);
        assert_eq!(visible_cols(None, &fields, default_n(false, DEFAULT_COLS)).len(), DEFAULT_COLS);
    }

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
