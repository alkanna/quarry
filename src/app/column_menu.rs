//! The column header popover: sort, alignment, width presets, data actions and writes sent to the Shell.

use super::*;
use crate::db::{COL_VALUES_CAP, ColOp, ColOut, col_op_result};

pub(super) const COL_MIN_W: f32 = 50.0;
pub(super) const COL_MAX_W: f32 = 900.0;

pub(super) enum WidthPreset {
    Min,
    Optimal,
    Max,
}

/// What the popover asked for, applied once the table is drawn.
pub(super) enum HeadAct {
    Sort(Option<i64>),
    Width(WidthPreset),
    Data(ColOp),
    /// Rename in the documents matching the filter (true) or in the whole collection.
    Rename(bool),
    Cast(&'static str),
    Update,
    Customize,
    Col(ColAct),
}

const CASTS: [(&str, &str); 8] = [("string", "String"), ("int", "Int32"), ("long", "Int64"), ("double", "Double"), ("decimal", "Decimal128"), ("bool", "Boolean"), ("date", "Date"), ("objectId", "ObjectId")];

/// The filter plus "this field exists", kept as two clauses so a condition the filter already has on
/// the field still applies.
fn with_exists(filter: Document, c: &str) -> Document {
    let has = doc! { c: { "$exists": true } };
    if filter.is_empty() { has } else { doc! { "$and": [filter, has] } }
}

/// True when a segment is an array position, which $rename and aggregation paths cannot reach.
fn indexed(c: &str) -> bool {
    c.split('.').any(|s| s.parse::<usize>().is_ok())
}

/// Names the collection the values came from, not whichever tab is current when a result arrives.
fn col_title(agg: bool, op: ColOp, coll: &str, path: &str) -> String {
    if agg { format!("{}: {path} in the results", op.label()) } else { format!("{}: {coll}.{path}", op.label()) }
}

fn update_script(coll: &str, filter: Document, update: Bson, note: &str) -> String {
    format!(
        "async function run() {{\n  // {note}\n  return await {}.updateMany(\n    {},\n    {}\n  );\n}}",
        collection_ref(coll),
        db::pretty_shell(&Bson::Document(filter)),
        db::pretty_shell(&update)
    )
}

#[derive(Clone, Copy)]
enum Icon {
    SortAsc,
    SortDesc,
    Unsort,
    AlignL,
    AlignC,
    AlignR,
    WMin,
    WOpt,
    WMax,
}

/// One painted cell of the popover's icon grid.
fn grid_icon(ui: &mut egui::Ui, w: f32, icon: Icon, on: bool, enabled: bool, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 30.0), if enabled { Sense::click() } else { Sense::hover() });
    let r = if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
    let p = ui.painter();
    if on {
        p.rect_filled(rect.shrink(2.0), R_SM, tint(PRIMARY(), 40));
    } else if enabled && r.hovered() {
        p.rect_filled(rect.shrink(2.0), R_SM, HOVER());
    }
    let s = Stroke::new(1.5, if !enabled { FAINT().gamma_multiply(0.5) } else if on { PRIMARY() } else { MUTED() });
    let c = rect.center();
    let h = |x0: f32, x1: f32, y: f32| p.line_segment([c + egui::vec2(x0, y), c + egui::vec2(x1, y)], s);
    let v = |x: f32| p.line_segment([c + egui::vec2(x, -6.0), c + egui::vec2(x, 6.0)], s);
    let arrow = |x0: f32, x1: f32| {
        h(x0, x1, 0.0);
        let d = (x1 - x0).signum() * 3.0;
        h(x1 - d, x1, -3.0);
        h(x1 - d, x1, 3.0);
    };
    match icon {
        Icon::SortAsc | Icon::SortDesc | Icon::Unsort => {
            for (i, y) in [-5.0, 0.0, 5.0].into_iter().enumerate() {
                let len = match icon {
                    Icon::SortAsc => 4.0 + 5.0 * i as f32,
                    Icon::SortDesc => 14.0 - 5.0 * i as f32,
                    _ => 14.0,
                };
                h(-7.0, -7.0 + len, y);
            }
        }
        Icon::AlignL | Icon::AlignC | Icon::AlignR => {
            for (i, y) in [-6.0, -2.0, 2.0, 6.0].into_iter().enumerate() {
                let len = if i % 2 == 0 { 16.0 } else { 10.0 };
                let x0 = match icon {
                    Icon::AlignL => -8.0,
                    Icon::AlignC => -len / 2.0,
                    _ => 8.0 - len,
                };
                h(x0, x0 + len, y);
            }
        }
        Icon::WMin => {
            v(0.0);
            arrow(-11.0, -3.0);
            arrow(11.0, 3.0);
        }
        Icon::WOpt => {
            v(-7.0);
            v(7.0);
            h(-7.0, 7.0, 0.0);
        }
        Icon::WMax => {
            v(-11.0);
            v(11.0);
            arrow(-2.0, -8.0);
            arrow(2.0, 8.0);
        }
    }
    r.on_hover_text(tip)
}

fn icon_row(ui: &mut egui::Ui, items: [(Icon, bool, bool, &str, HeadAct); 3], acts: &mut Vec<HeadAct>) {
    let w = (ui.available_width() / 3.0).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, (icon, on, enabled, tip, act)) in items.into_iter().enumerate() {
            let r = grid_icon(ui, w, icon, on, enabled, tip);
            if i < 2 {
                ui.painter().vline(r.rect.right(), r.rect.shrink(6.0).y_range(), Stroke::new(1.0, BORDER()));
            }
            if r.clicked() {
                acts.push(act);
                ui.close();
            }
        }
    });
}

/// Contents of a column header's popover; `dir` is the column's current sort direction.
pub(super) fn col_popover(ui: &mut egui::Ui, c: &str, agg: bool, dir: Option<i64>, prefs: &ColPrefs, scope_query: &mut bool, acts: &mut Vec<HeadAct>) {
    menu_look(ui);
    ui.add_space(4.0);
    ui.vertical_centered(|ui| caption(ui, "COLUMN"));
    ui.add_space(4.0);
    let align = prefs.align.get(c).copied().unwrap_or(0);
    let width = prefs.widths.get(c).copied();
    icon_row(ui, [
        (Icon::SortAsc, dir.is_some_and(|d| d > 0), !agg, "Sort ascending", HeadAct::Sort(Some(1))),
        (Icon::SortDesc, dir.is_some_and(|d| d < 0), !agg, "Sort descending", HeadAct::Sort(Some(-1))),
        (Icon::Unsort, false, !agg && dir.is_some(), "Unsort", HeadAct::Sort(None)),
    ], acts);
    icon_row(ui, [
        (Icon::AlignL, align == 0, true, "Align left", HeadAct::Col(ColAct::Align(c.into(), 0))),
        (Icon::AlignC, align == 1, true, "Align center", HeadAct::Col(ColAct::Align(c.into(), 1))),
        (Icon::AlignR, align == 2, true, "Align right", HeadAct::Col(ColAct::Align(c.into(), 2))),
    ], acts);
    icon_row(ui, [
        (Icon::WMin, width == Some(COL_MIN_W), true, "Minimum width", HeadAct::Width(WidthPreset::Min)),
        (Icon::WOpt, width.is_none(), true, "Optimal width", HeadAct::Width(WidthPreset::Optimal)),
        (Icon::WMax, false, true, "Maximum width: the widest value", HeadAct::Width(WidthPreset::Max)),
    ], acts);
    ui.separator();
    ui.menu_button("Data", |ui| {
        menu_look(ui);
        if !agg {
            segmented(ui, scope_query, &[(false, "This page"), (true, "Whole query")]);
            ui.add_space(4.0);
        }
        for op in ColOp::ALL {
            item(ui, op.label(), HeadAct::Data(op), acts);
        }
    });
    // Aggregation results are not documents of the collection, so there is nothing to write back to.
    if !agg {
        ui.add_enabled_ui(!indexed(c), |ui| {
            let why = "Not available for a path through an array position";
            ui.menu_button("Rename field", |ui| {
                menu_look(ui);
                for (label, filtered) in [("Filtered documents", true), ("All documents in collection", false)] {
                    item(ui, label, HeadAct::Rename(filtered), acts);
                }
            }).response.on_disabled_hover_text(why);
            ui.menu_button("Cast values to", |ui| {
                menu_look(ui);
                for (to, label) in CASTS {
                    item(ui, label, HeadAct::Cast(to), acts);
                }
            }).response.on_disabled_hover_text(why);
        });
        item(ui, "Update values to…", HeadAct::Update, acts);
    }
    ui.separator();
    item(ui, "Customize…", HeadAct::Customize, acts);
    ui.separator();
    let pinned = prefs.pinned.iter().any(|p| p == c);
    for (label, act) in [
        (if pinned { "Unpin" } else { "Pin to the left" }, ColAct::Pin(c.into())),
        ("Move left", ColAct::Move(c.into(), -1)),
        ("Move right", ColAct::Move(c.into(), 1)),
        ("Hide column", ColAct::Hide(c.into())),
    ] {
        item(ui, label, HeadAct::Col(act), acts);
    }
    if ui.checkbox(&mut prefs.highlight.contains(&db::strip_indices(c)), "Highlight column").clicked() {
        acts.push(HeadAct::Col(ColAct::Highlight(c.into())));
        ui.close();
    }
}

/// A popover entry that records its action and closes the popover.
fn item(ui: &mut egui::Ui, label: &str, act: HeadAct, acts: &mut Vec<HeadAct>) {
    if ui.button(label).clicked() {
        acts.push(act);
        ui.close();
    }
}

impl App {
    /// Applies one popover action for column `c` of the current table.
    pub(super) fn head_act(&mut self, ui: &egui::Ui, agg: bool, docs: &[Document], fields: &[String], c: &str, act: HeadAct) {
        let key = self.col_key(agg);
        let Some(t) = self.t() else { return };
        let label = self.prefs.cols.get(&key).map(|p| p.label(c).to_string()).unwrap_or_else(|| c.into());
        match act {
            HeadAct::Sort(next) => {
                let t = self.tm().unwrap();
                t.query.sort = next.map(|d| doc! { c: d });
                t.sort_txt = t.query.sort.as_ref().map(|s| db::shell_text(&Bson::Document(s.clone()))).unwrap_or_default();
                t.page = 0;
                self.reload();
            }
            HeadAct::Col(a) => self.apply_col_acts(&key, fields, vec![a]),
            HeadAct::Width(wp) => {
                let w = match wp {
                    WidthPreset::Min => Some(COL_MIN_W),
                    WidthPreset::Optimal => None,
                    WidthPreset::Max => Some(super::table::optimal_width(ui, docs, c, &label, !self.prefs.object_preview, COL_MAX_W).min(COL_MAX_W)),
                };
                let widths = &mut self.prefs.cols.entry(key).or_default().widths;
                match w {
                    Some(w) => widths.insert(c.into(), w),
                    None => widths.remove(c),
                };
                self.save_prefs();
            }
            HeadAct::Data(op) => self.col_data(ui.ctx(), agg, docs, c, op),
            HeadAct::Rename(filtered) => {
                let f = with_exists(if filtered { t.query.filter.clone() } else { Document::new() }, c);
                let to = match c.rsplit_once('.') {
                    Some((parent, _)) => format!("{parent}.NEW_NAME"),
                    None => "NEW_NAME".into(),
                };
                let code = update_script(&t.sel.coll, f, Bson::Document(doc! { "$rename": { c: to } }), "Replace NEW_NAME, then run.");
                self.open_in_shell(code);
            }
            HeadAct::Cast(to) => {
                let input = format!("${c}");
                // The exists clause keeps documents without the field from gaining it as null.
                let stage = doc! { "$set": { c: { "$convert": { "input": &input, "to": to, "onError": &input, "onNull": Bson::Null } } } };
                let note = format!("Converts {c} to {to} in the documents matching the filter; values that cannot convert are kept.");
                let code = update_script(&t.sel.coll, with_exists(t.query.filter.clone(), c), Bson::Array(vec![Bson::Document(stage)]), &note);
                self.open_in_shell(code);
            }
            HeadAct::Update => {
                let note = format!("Replace null with the new value of {c}, then run.");
                let code = update_script(&t.sel.coll, t.query.filter.clone(), Bson::Document(doc! { "$set": { c: Bson::Null } }), &note);
                self.open_in_shell(code);
            }
            HeadAct::Customize => {
                let label = if label == c { String::new() } else { label };
                self.modal = Some(Modal::ColumnCustomize { key, agg, col: c.into(), label, path: c.into() });
            }
        }
    }

    fn col_data(&mut self, ctx: &egui::Context, agg: bool, docs: &[Document], c: &str, op: ColOp) {
        let Some(t) = self.t() else { return };
        let (sel, filter) = (t.sel.clone(), t.query.filter.clone());
        if agg || !self.col_scope_query {
            // As the table shows them, unsaved edits included.
            let values: Vec<Bson> = docs.iter().filter_map(|d| {
                let shown = self.shown_doc(d, agg);
                db::get_path(shown.as_ref().unwrap_or(d), c).cloned()
            }).collect();
            let scope = format!("From the {} documents on this page.", docs.len());
            return match col_op_result(op, &values) {
                ColOut::Groups(rows) => {
                    // Pipeline and script results are not the collection, so there is nothing to reopen.
                    let pipeline = if agg { Vec::new() } else { db::group_count_pipeline(c, &Bson::Null, filter).unwrap_or_default() };
                    self.modal = Some(Modal::Grouped { sel, path: c.into(), pipeline, rows });
                }
                out => self.col_result(ctx, &col_title(agg, op, &sel.coll, c), c, out, &scope),
            };
        }
        let Some(target) = self.target() else { return self.err("Not connected.") };
        let sort = self.t().unwrap().query.sort.clone();
        self.ok(format!("Reading {c} from every matching document…"));
        self.worker.column_values(sel.conn, target, op, c.into(), filter, sort);
    }

    /// Copies or shows a data action's result. Groups never get here: they open `Modal::Grouped`.
    fn col_result(&mut self, ctx: &egui::Context, title: &str, path: &str, out: ColOut, scope: &str) {
        match out {
            ColOut::Copy(text, n) => {
                ctx.copy_text(text);
                self.ok(format!("Copied {n} values of {path}"));
            }
            ColOut::Notice(body) => self.modal = Some(Modal::Text { title: title.into(), body: format!("{body}\n\n{scope}"), code: false }),
            ColOut::Groups(_) => {}
        }
    }

    pub(super) fn column_values_done(&mut self, ctx: &egui::Context, op: ColOp, coll: String, path: String, res: db::Res<(ColOut, bool)>) {
        match res {
            Ok((out, capped)) => {
                let scope = if capped { format!("From the first {COL_VALUES_CAP} documents of the query, in its sort order.") } else { "From every document matching the query.".into() };
                self.col_result(ctx, &col_title(false, op, &coll, &path), &path, out, &scope);
            }
            Err(e) => self.err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_scripts_keep_the_filter() {
        assert_eq!(with_exists(Document::new(), "a"), doc! { "a": { "$exists": true } });
        // The filter's own condition on the field must survive the exists clause.
        assert_eq!(with_exists(doc! { "a": "x" }, "a"), doc! { "$and": [{ "a": "x" }, { "a": { "$exists": true } }] });
        assert!(indexed("items.0.sku") && !indexed("items.sku"));
    }
}
