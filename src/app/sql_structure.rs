//! The read-only SQL views about one table: Structure, Relations, Triggers and Info. They share one
//! fetch (`db::sql::Structure`), asked for when a view first draws and again on refresh. Editing
//! the structure comes with writes (docs/sql/PLAN.md, v2).

use super::*;
use db::sql::structure::{Column, ForeignKey, Index, Structure, Trigger};
use egui::{pos2, vec2, Rect};

const READ_ONLY: &str = "Read-only: unlock writes to change the structure";
const LATER: &str = "Comes with writes";

/// What the four views keep per tab.
#[derive(Default)]
pub(super) struct StructState {
    load: Load,
    data: Option<Structure>,
    /// The selected row of each grid: columns, indexes, keys out, keys in, triggers.
    sel: [Option<usize>; 5],
}

#[derive(Default)]
enum Load {
    #[default]
    Idle,
    Loading,
    Failed(String),
}

impl StructState {
    pub(super) fn finish(&mut self, res: db::Res<Structure>) {
        match res {
            Ok(s) => (self.data, self.load) = (Some(s), Load::Idle),
            Err(e) => self.load = Load::Failed(e),
        }
    }
}

/// What a frame of drawing asks the app to do afterwards.
enum Act {
    Copy(String),
    Open { db: String, table: String },
    Refresh,
}

impl App {
    /// Asks for the current tab's structure. The answer is matched back by `idx_req`.
    fn request_structure(&mut self) {
        let i = self.cur;
        let Some(db) = self.tabs.get(i).and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.clone()) else { return };
        self.req += 1;
        let req = self.req;
        let t = &mut self.tabs[i];
        t.idx_req = req;
        if let Some(s) = t.sql.as_mut() {
            s.st.load = Load::Loading;
        }
        self.worker.sql_structure(req, db, t.sel.db.clone(), t.sel.coll.clone());
    }

    /// The loaded structure, taken out of the tab while the view draws it (put back with `keep`).
    /// Draws the wait or the error and returns None until there is data.
    fn take_structure(&mut self, ui: &mut egui::Ui) -> Option<(Structure, [Option<usize>; 5])> {
        let st = &self.tabs[self.cur].sql.as_ref()?.st;
        if st.data.is_none() && matches!(st.load, Load::Idle) {
            self.request_structure();
        }
        let st = &mut self.tabs[self.cur].sql.as_mut()?.st;
        match (st.data.take(), &st.load) {
            (Some(d), _) => Some((d, st.sel)),
            (None, Load::Failed(e)) => {
                let e = e.clone();
                ui.add_space(24.0);
                ui.vertical_centered(|ui| ui.label(RichText::new(e).size(TXT_SMALL).color(ALERT())));
                if quiet(ui, "Retry").clicked() {
                    self.act(ui.ctx(), Act::Refresh);
                }
                None
            }
            (None, _) => {
                ui.add_space(24.0);
                ui.vertical_centered(spin);
                None
            }
        }
    }

    fn keep(&mut self, ctx: &egui::Context, data: Structure, sel: [Option<usize>; 5], acts: Vec<Act>) {
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            (s.st.data, s.st.sel) = (Some(data), sel);
        }
        for a in acts {
            self.act(ctx, a);
        }
    }

    fn act(&mut self, ctx: &egui::Context, a: Act) {
        match a {
            Act::Copy(text) => ctx.copy_text(text),
            Act::Open { db, table } => {
                let conn = self.tabs[self.cur].sel.conn;
                self.open_collection(Sel { conn, db, coll: table }, false);
            }
            Act::Refresh => {
                if let Some(s) = self.tabs[self.cur].sql.as_mut() {
                    s.st.load = Load::Idle;
                }
                self.request_structure();
            }
        }
    }

    /// The count shown beside a view's tab once the structure is known.
    pub(super) fn sql_view_count(&self, v: View) -> Option<usize> {
        let d = self.t()?.sql.as_ref()?.st.data.as_ref()?;
        Some(match v {
            View::Relations => d.foreign_keys.len(),
            View::Triggers => d.triggers.len(),
            _ => 0,
        })
        .filter(|n| *n > 0)
    }

    pub(super) fn sql_structure_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, mut sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        edit_row(ui, &["+ Column", "Duplicate", "Delete"], READ_ONLY, |ui| {
            if quiet(ui, "Copy CREATE TABLE").clicked() {
                acts.push(Act::Copy(d.create.clone()));
            }
            if refresh_button(ui) {
                acts.push(Act::Refresh);
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().id_salt("sql-structure").auto_shrink([false, false]).show(ui, |ui| {
            let heads = [gc("Name", 1.5), gc("Type", 1.0), gc("Length / values", 1.4), gc("Unsigned", 0.95), gc("Null", 0.6), gc("Key", 0.6), gc("Default", 1.9), gc("Extra", 2.1), gc("Collation", 1.9), gc("Comment", 1.3)];
            if let Some(r) = grid(ui, "columns", &heads, d.columns.len(), sel[0], |ui, rect, r, c| column_cell(ui, rect, &d.columns[r], c)) {
                sel[0] = Some(r);
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                caption(ui, "INDEXES");
                stat_chip(ui, &d.indexes.len().to_string());
                right_group(ui, |ui| {
                    ui.add_enabled_ui(false, |ui| quiet(ui, "+ Index")).response.on_disabled_hover_text(READ_ONLY);
                });
            });
            ui.add_space(6.0);
            let heads = [gc("Key name", 2.0), gc("Columns", 2.4), gc("Unique", 0.8), gc("Type", 0.8), gc_r("Cardinality", 1.2), gc("Comment", 2.4)];
            if let Some(r) = grid(ui, "indexes", &heads, d.indexes.len(), sel[1], |ui, rect, r, c| index_cell(ui, rect, &d.indexes[r], c)) {
                sel[1] = Some(r);
            }
        });
        self.keep(&ui.ctx().clone(), d, sel, acts);
    }

    pub(super) fn sql_relations_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, mut sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        let mut open = None;
        edit_row(ui, &["+ Relation", "Delete"], READ_ONLY, |ui| {
            if refresh_button(ui) {
                acts.push(Act::Refresh);
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().id_salt("sql-relations").auto_shrink([false, false]).show(ui, |ui| {
            caption(ui, "THIS TABLE POINTS TO");
            ui.add_space(6.0);
            let heads = [gc("Name", 2.0), gc("Column", 1.6), gc("References", 2.0), gc("On update", 1.2), gc("On delete", 1.2), gc("Index", 2.0)];
            if let Some(r) = grid(ui, "fk-out", &heads, d.foreign_keys.len(), sel[2], |ui, rect, r, c| {
                let fk = &d.foreign_keys[r];
                match c {
                    0 => put(ui, rect, &fk.name, mono(TXT_SMALL), TEXT(), false),
                    1 => put(ui, rect, &fk.cols.join(", "), mono(TXT_SMALL), TEXT(), false),
                    2 => {
                        if link(ui, rect, ("fk-out-link", r), &format!("{}.{}", fk.ref_table, fk.ref_cols.join(", "))) {
                            open = Some((fk.ref_db.clone(), fk.ref_table.clone()));
                        }
                    }
                    3 => put(ui, rect, &fk.on_update, mono(TXT_SMALL), C_KEYWORD(), false),
                    4 => put(ui, rect, &fk.on_delete, mono(TXT_SMALL), C_KEYWORD(), false),
                    _ => put(ui, rect, d.index_for(fk).map_or("", |i| i.name.as_str()), mono(TXT_SMALL), MUTED(), false),
                }
            }) {
                sel[2] = Some(r);
            }
            ui.add_space(16.0);
            caption(ui, "TABLES THAT POINT HERE");
            ui.add_space(6.0);
            let heads = [gc("Table", 2.0), gc("Column", 1.6), gc("Constraint", 2.0), gc("On update", 1.2), gc("On delete", 1.2)];
            if let Some(r) = grid(ui, "fk-in", &heads, d.referenced_by.len(), sel[3], |ui, rect, r, c| {
                let fk = &d.referenced_by[r];
                match c {
                    0 => {
                        if link(ui, rect, ("fk-in-link", r), &fk.table) {
                            open = Some((fk.db.clone(), fk.table.clone()));
                        }
                    }
                    1 => put(ui, rect, &fk.cols.join(", "), mono(TXT_SMALL), TEXT(), false),
                    2 => put(ui, rect, &fk.name, mono(TXT_SMALL), MUTED(), false),
                    3 => put(ui, rect, &fk.on_update, mono(TXT_SMALL), C_KEYWORD(), false),
                    _ => put(ui, rect, &fk.on_delete, mono(TXT_SMALL), C_KEYWORD(), false),
                }
            }) {
                sel[3] = Some(r);
            }
        });
        acts.extend(open.map(|(db, table)| Act::Open { db, table }));
        self.keep(&ui.ctx().clone(), d, sel, acts);
    }

    pub(super) fn sql_triggers_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, mut sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        edit_row(ui, &["+ Trigger", "Delete"], READ_ONLY, |ui| {
            if refresh_button(ui) {
                acts.push(Act::Refresh);
            }
        });
        ui.add_space(8.0);
        let heads = [gc("Name", 2.0), gc("Timing", 0.9), gc("Event", 0.9), gc("Definer", 1.5), gc("Statement", 4.0)];
        if let Some(r) = grid(ui, "triggers", &heads, d.triggers.len(), sel[4], |ui, rect, r, c| {
            let t = &d.triggers[r];
            match c {
                0 => put(ui, rect, &t.name, mono(TXT_SMALL), TEXT(), false),
                1 => put(ui, rect, &t.timing, mono(TXT_SMALL), C_KEYWORD(), false),
                2 => put(ui, rect, &t.event, mono(TXT_SMALL), C_KEYWORD(), false),
                3 => put(ui, rect, &t.definer, mono(TXT_SMALL), MUTED(), false),
                _ => put(ui, rect, &preview(t), mono(TXT_SMALL), FAINT(), false),
            }
        }) {
            sel[4] = Some(r);
        }
        let shown = sel[4].filter(|i| *i < d.triggers.len()).or(if d.triggers.is_empty() { None } else { Some(0) });
        ui.add_space(12.0);
        match shown {
            Some(i) => code_view(ui, "trigger-code", &d.triggers[i].create_sql(&self.tabs[self.cur].sel.coll), ui.available_height() - 36.0),
            None => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("This table has no triggers").size(TXT_SMALL).color(FAINT())));
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Read-only: unlock writes to change triggers").size(TXT_SMALL).color(FAINT()));
        self.keep(&ui.ctx().clone(), d, sel, acts);
    }

    pub(super) fn sql_info_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        let s = &d.status;
        let none = || "-".to_string();
        let bytes = |n: Option<u64>| n.map_or_else(none, size);
        let rows = [
            ("Engine", s.engine.clone()),
            ("Row format", s.row_format.clone()),
            ("Collation", s.collation.clone()),
            ("Rows (estimate)", s.rows.map_or_else(none, |n| format!("{}{} ", if s.engine == "InnoDB" { "~" } else { "" }, group(&n.to_string())) + if s.engine == "InnoDB" { "(estimate)" } else { "" })),
            ("Average row", bytes(s.avg_row)),
            ("Data", bytes(s.data)),
            ("Indexes", bytes(s.index)),
            ("Free", bytes(s.free)),
            ("Auto increment", s.auto_increment.map_or_else(none, |n| group(&n.to_string()))),
            ("Created", if s.created.is_empty() { none() } else { s.created.clone() }),
            ("Updated", if s.updated.is_empty() { none() } else { s.updated.clone() }),
            ("Comment", s.comment.clone()),
        ];
        let h = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.allocate_ui_with_layout(vec2(380.0_f32.min(ui.available_width() * 0.4), h), egui::Layout::top_down(egui::Align::Min), |ui| {
                card(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for (k, v) in &rows {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(112.0, 26.0), Sense::hover());
                            put(ui, r, k, FontId::proportional(TXT_BODY), MUTED(), false);
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                            put(ui, r, v, FontId::monospace(TXT_BODY), if v == "-" { FAINT() } else { BRIGHT() }, false);
                        });
                    }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    for name in ["Analyze", "Optimize", "Check"] {
                        ui.add_enabled_ui(false, |ui| ui.button(name)).response.on_disabled_hover_text(LATER);
                    }
                    ui.add_enabled_ui(false, |ui| danger(ui, "Truncate...")).response.on_disabled_hover_text(LATER);
                });
            });
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    caption(ui, "CREATE TABLE");
                    right_group(ui, |ui| {
                        if quiet(ui, "Copy").clicked() {
                            acts.push(Act::Copy(d.create.clone()));
                        }
                        if refresh_button(ui) {
                            acts.push(Act::Refresh);
                        }
                    });
                });
                ui.add_space(4.0);
                code_view(ui, "info-code", &d.create, ui.available_height() - 4.0);
            });
        });
        self.keep(&ui.ctx().clone(), d, sel, acts);
    }
}

fn refresh_button(ui: &mut egui::Ui) -> bool {
    icon_button(ui, "⟳", true).on_hover_text("Reload the structure").clicked()
}

/// The row above a grid: actions that need writes, shown disabled, then a note, then `right`.
fn edit_row(ui: &mut egui::Ui, buttons: &[&str], note: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for b in buttons {
            quiet_enabled(ui, false, *b).on_disabled_hover_text(READ_ONLY);
        }
        ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
        right_group(ui, right);
    });
}

/// "1.4 MB" for a byte count, one decimal from kilobytes up.
fn size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

/// The first line of a trigger body, shown in its grid row.
fn preview(t: &Trigger) -> String {
    let body = t.body.trim();
    // A BEGIN ... END body starts with the statement that matters on its second line.
    let skip = if body.to_ascii_uppercase().starts_with("BEGIN") { 1 } else { 0 };
    let line = body.lines().map(str::trim).filter(|l| !l.is_empty()).nth(skip).unwrap_or_default();
    if body.lines().count() > skip + 1 { format!("{line} ...") } else { line.to_string() }
}

struct GCol {
    head: &'static str,
    weight: f32,
    right: bool,
}

fn gc(head: &'static str, weight: f32) -> GCol {
    GCol { head, weight, right: false }
}

fn gc_r(head: &'static str, weight: f32) -> GCol {
    GCol { head, weight, right: true }
}

fn put(ui: &egui::Ui, cell: Rect, s: &str, font: FontId, color: Color32, right: bool) {
    place(ui, cell, s, font, color, right);
}

/// Text cut to its cell with an ellipsis, centered on its ink. Returns where it ended up.
fn place(ui: &egui::Ui, cell: Rect, s: &str, font: FontId, color: Color32, right: bool) -> Rect {
    let area = cell.shrink2(vec2(8.0, 0.0));
    let mut job = egui::text::LayoutJob::single_section(s.to_string(), egui::TextFormat { font_id: font, color, ..Default::default() });
    job.wrap = egui::text::TextWrapping { max_width: area.width().max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = ui.painter().layout_job(job);
    let x = if right { area.right() - g.size().x } else { area.left() };
    let pos = pos2(x, cell.center().y - g.size().y / 2.0 + optical_dy(&g));
    let rect = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color);
    rect
}

/// A name that opens another table. True when clicked.
fn link(ui: &mut egui::Ui, cell: Rect, id: (&str, usize), s: &str) -> bool {
    let area = cell.shrink2(vec2(8.0, 0.0));
    let g = ui.painter().layout_no_wrap(s.to_string(), FontId::monospace(TXT_SMALL), C_LINK());
    let hit = Rect::from_min_size(pos2(area.left(), cell.top() + 4.0), vec2(g.size().x.min(area.width()), cell.height() - 8.0));
    let r = ui.interact(hit, ui.scope_id().with(id), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let ink = place(ui, cell, s, FontId::monospace(TXT_SMALL), C_LINK(), false);
    if r.hovered() {
        ui.painter().hline(ink.x_range(), ink.bottom() + 0.5, Stroke::new(1.0, C_LINK()));
    }
    r.clicked()
}

/// A read-only check mark in a box, for flags like Unsigned and Unique.
fn tick(ui: &egui::Ui, rect: Rect, on: bool) {
    let c = pos2(rect.left() + 16.0, rect.center().y);
    let b = Rect::from_center_size(c, vec2(14.0, 14.0));
    let p = ui.painter();
    p.rect(b, R_SM, if on { PRIMARY() } else { BG() }, Stroke::new(1.0, if on { PRIMARY() } else { BORDER() }), egui::StrokeKind::Inside);
    if on {
        paint_check(p, c, ON_PRIMARY());
    }
}

/// "PK" in front of a name, then the name.
fn named(ui: &egui::Ui, cell: Rect, name: &str, pk: bool, font: FontId) {
    if pk {
        put(ui, cell, "PK", FontId::monospace(TXT_CAPTION), WARNING(), false);
        put(ui, Rect::from_min_max(pos2(cell.left() + 24.0, cell.top()), cell.max), name, font, BRIGHT(), false);
    } else {
        put(ui, cell, name, font, TEXT(), false);
    }
}

fn column_cell(ui: &egui::Ui, rect: Rect, c: &Column, col: usize) {
    let mono = || FontId::monospace(TXT_SMALL);
    let (base, args, unsigned) = (c.base_type(), c.args(), c.unsigned());
    match col {
        0 => named(ui, rect, &c.name, c.key == "PRI", FontId::proportional(TXT_BODY)),
        1 => {
            put(ui, rect, &base, mono(), C_KEYWORD(), false);
        }
        2 => {
            let color = if args.starts_with('\'') { C_STRING() } else { C_NUMBER() };
            put(ui, rect, &args, mono(), color, false);
        }
        3 => tick(ui, rect, unsigned),
        4 => tick(ui, rect, c.nullable),
        5 => {
            let color = if c.key == "PRI" { WARNING() } else { C_LINK() };
            put(ui, rect, &c.key, mono(), color, false);
        }
        6 => match &c.default {
            None => {
                let g = ui.painter().layout_no_wrap("NULL".into(), FontId::monospace(TXT_SMALL), FAINT());
                let at = pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g));
                // Italic is not embedded, so the NULL leans by shearing is skipped: faint says enough.
                ui.painter().galley(at, g, FAINT());
            }
            Some(d) if c.default_expr => {
                put(ui, rect, d, mono(), C_KEYWORD(), false);
            }
            Some(d) if matches!(base.as_str(), "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" | "DECIMAL" | "NUMERIC" | "FLOAT" | "DOUBLE" | "BIT" | "YEAR") => {
                put(ui, rect, d, mono(), C_NUMBER(), false);
            }
            Some(d) => {
                put(ui, rect, &format!("'{d}'"), mono(), C_STRING(), false);
            }
        },
        7 => {
            let extra = c.extra.replace("DEFAULT_GENERATED", "");
            put(ui, rect, extra.trim(), mono(), TEXT(), false);
        }
        8 => {
            put(ui, rect, c.collation.as_deref().unwrap_or(""), mono(), MUTED(), false);
        }
        _ => {
            put(ui, rect, &c.comment, FontId::proportional(TXT_BODY), TEXT(), false);
        }
    }
}

fn index_cell(ui: &egui::Ui, rect: Rect, ix: &Index, col: usize) {
    let mono = || FontId::monospace(TXT_SMALL);
    match col {
        0 => named(ui, rect, &ix.name, ix.name == "PRIMARY", mono()),
        1 => {
            let cols: Vec<String> = ix.cols.iter().map(|c| format!("{}{}{}", c.name, c.prefix.map_or(String::new(), |n| format!("({n})")), if c.desc { " DESC" } else { "" })).collect();
            put(ui, rect, &cols.join(", "), mono(), TEXT(), false);
        }
        2 => tick(ui, rect, ix.unique),
        3 => {
            put(ui, rect, &ix.kind, mono(), MUTED(), false);
        }
        4 => {
            put(ui, rect, &ix.cardinality.map_or(String::new(), |n| group(&n.to_string())), mono(), C_NUMBER(), true);
        }
        _ => {
            put(ui, rect, &ix.comment, FontId::proportional(TXT_BODY), TEXT(), false);
        }
    }
}

/// A bordered table with a header row, striped rows and a row number. `cell` paints one cell and
/// may add widgets over it. Returns the row that was clicked.
fn grid(ui: &mut egui::Ui, salt: &str, cols: &[GCol], rows: usize, sel: Option<usize>, mut cell: impl FnMut(&mut egui::Ui, Rect, usize, usize)) -> Option<usize> {
    const NUM: f32 = 40.0;
    let w = ui.available_width();
    let total: f32 = cols.iter().map(|c| c.weight).sum();
    let unit = (w - NUM) / total;
    let (rect, _) = ui.allocate_exact_size(vec2(w, TABLE_HEAD_H + rows as f32 * ROW_H), Sense::hover());
    let top = egui::CornerRadius { nw: R_MD, ne: R_MD, sw: 0, se: 0 };
    ui.painter().rect_filled(rect, R_MD, BG());
    ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(w, TABLE_HEAD_H)), top, HDR());
    let mut x = rect.left() + NUM;
    let xs: Vec<(f32, f32)> = cols
        .iter()
        .map(|c| {
            let a = x;
            x += c.weight * unit;
            (a, x)
        })
        .collect();
    for (c, (a, b)) in cols.iter().zip(&xs) {
        let head = Rect::from_min_max(pos2(*a, rect.top()), pos2(*b, rect.top() + TABLE_HEAD_H));
        put(ui, head, c.head, FontId::proportional(TXT_SMALL), MUTED(), c.right);
    }
    let stripe = ui.visuals().faint_bg_color;
    let mut clicked = None;
    for r in 0..rows {
        let row = Rect::from_min_size(pos2(rect.left(), rect.top() + TABLE_HEAD_H + r as f32 * ROW_H), vec2(w, ROW_H));
        let resp = ui.interact(row, ui.scope_id().with(("sql-grid", salt, r)), Sense::click());
        let fill = if sel == Some(r) { if resp.hovered() { SEL_HOVER() } else { SEL() } } else if resp.hovered() { HOVER() } else if r % 2 == 1 { stripe } else { BG() };
        ui.painter().rect_filled(row, 0.0, fill);
        put(ui, Rect::from_min_size(row.min, vec2(NUM, ROW_H)), &(r + 1).to_string(), FontId::monospace(TXT_CAPTION), FAINT(), false);
        for (i, (a, b)) in xs.iter().enumerate() {
            cell(ui, Rect::from_min_max(pos2(*a, row.top()), pos2(*b, row.bottom())), r, i);
        }
        if resp.clicked() {
            clicked = Some(r);
        }
    }
    ui.painter().hline(rect.x_range(), rect.top() + TABLE_HEAD_H, Stroke::new(1.0, LINE()));
    ui.painter().rect_stroke(rect, R_MD, Stroke::new(1.0, LINE()), egui::StrokeKind::Inside);
    clicked
}

/// A read-only code area with line numbers, scrolling both ways inside `height`.
fn code_view(ui: &mut egui::Ui, salt: &str, text: &str, height: f32) {
    const GUTTER: f32 = 44.0;
    const LINE_H: f32 = 20.0;
    let galleys: Vec<_> = split_lines(&sql_job(text, TXT_BODY)).into_iter().map(|j| ui.painter().layout_job(j)).collect();
    let widest = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max);
    let h = galleys.len() as f32 * LINE_H + 16.0;
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::both().id_salt(salt).max_height(height.clamp(80.0, h)).auto_shrink([false, true]).show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2((GUTTER + widest + 24.0).max(ui.available_width()), h), Sense::hover());
            let p = ui.painter();
            for (i, g) in galleys.iter().enumerate() {
                let y = rect.top() + 8.0 + i as f32 * LINE_H;
                let n = p.layout_no_wrap((i + 1).to_string(), FontId::monospace(TXT_SMALL), FAINT());
                p.galley(pos2(rect.left() + GUTTER - 12.0 - n.size().x, y + (LINE_H - n.size().y) / 2.0), n, FAINT());
                p.galley(pos2(rect.left() + GUTTER + 4.0, y + (LINE_H - g.size().y) / 2.0), g.clone(), TEXT());
            }
        });
    });
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

impl App {
    /// The orders table of docs/sql/designs.figcraft for the QUARRY_TEST_SQL demo: structure, info,
    /// relations or triggers pick the view. Never touches a server or the keychain.
    pub(super) fn sql_structure_demo(&mut self, which: &str) {
        let view = match which {
            "structure" => View::Structure,
            "info" => View::Info,
            "relations" => View::Relations,
            "triggers" => View::Triggers,
            _ => return,
        };
        let col = |name: &str, ty: &str, nullable: bool, key: &str, default: Option<&str>, expr: bool, extra: &str, coll: bool, comment: &str| Column {
            name: name.into(),
            full_type: ty.into(),
            nullable,
            key: key.into(),
            default: default.map(Into::into),
            default_expr: expr,
            extra: extra.into(),
            collation: coll.then(|| "utf8mb4_0900_ai_ci".to_string()),
            comment: comment.into(),
        };
        let ix = |name: &str, unique: bool, cols: &[(&str, bool)], card: u64, comment: &str| Index {
            name: name.into(),
            unique,
            cols: cols.iter().map(|(n, d)| db::sql::structure::IndexCol { name: (*n).into(), desc: *d, prefix: None }).collect(),
            kind: "BTREE".into(),
            cardinality: Some(card),
            comment: comment.into(),
        };
        let fk = |name: &str, table: &str, c: &str, rt: &str, rc: &str, upd: &str, del: &str| ForeignKey { name: name.into(), db: "shop".into(), table: table.into(), cols: vec![c.into()], ref_db: "shop".into(), ref_table: rt.into(), ref_cols: vec![rc.into()], on_update: upd.into(), on_delete: del.into() };
        let create = "CREATE TABLE `orders` (\n  `id` int unsigned NOT NULL AUTO_INCREMENT,\n  `customer_id` int unsigned NOT NULL,\n  `status` enum('pending','paid','shipped','refunded','cancelled')\n    NOT NULL DEFAULT 'pending',\n  `total` decimal(10,2) unsigned NOT NULL DEFAULT 0.00,\n  `currency` char(3) NOT NULL DEFAULT 'EUR',\n  `note` text,\n  `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,\n  `updated_at` datetime DEFAULT NULL ON UPDATE CURRENT_TIMESTAMP,\n  PRIMARY KEY (`id`),\n  KEY `idx_status` (`status`),\n  KEY `idx_customer_created` (`customer_id`,`created_at` DESC),\n  CONSTRAINT `fk_orders_customer` FOREIGN KEY (`customer_id`)\n    REFERENCES `customers` (`id`) ON UPDATE CASCADE\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
        let d = Structure {
            columns: vec![
                col("id", "int unsigned", false, "PRI", None, false, "auto_increment", false, ""),
                col("customer_id", "int unsigned", false, "MUL", None, false, "", false, "FK customers.id"),
                col("status", "enum('pending','paid','shipped','refunded','cancelled')", false, "MUL", Some("pending"), false, "", true, ""),
                col("total", "decimal(10,2) unsigned", false, "", Some("0.00"), false, "", false, "Incl. tax"),
                col("currency", "char(3)", false, "", Some("EUR"), false, "", true, "ISO 4217"),
                col("note", "text", true, "", None, false, "", true, ""),
                col("created_at", "datetime", false, "MUL", Some("CURRENT_TIMESTAMP"), true, "DEFAULT_GENERATED", false, ""),
                col("updated_at", "datetime", true, "", None, false, "on update CURRENT_TIMESTAMP", false, ""),
            ],
            indexes: vec![
                ix("PRIMARY", true, &[("id", false)], 4410, ""),
                ix("idx_status", false, &[("status", false)], 5, ""),
                ix("idx_customer_created", false, &[("customer_id", false), ("created_at", true)], 4402, "Recent orders per customer"),
                ix("fk_orders_customer", false, &[("customer_id", false)], 1190, "Made for the foreign key"),
            ],
            foreign_keys: vec![fk("fk_orders_customer", "orders", "customer_id", "customers", "id", "CASCADE", "RESTRICT"), fk("fk_orders_currency", "orders", "currency", "currencies", "code", "CASCADE", "SET NULL")],
            referenced_by: vec![fk("fk_items_order", "order_items", "order_id", "orders", "id", "CASCADE", "CASCADE"), fk("fk_payments_order", "payments", "order_id", "orders", "id", "CASCADE", "RESTRICT"), fk("fk_shipments_order", "shipments", "order_id", "orders", "id", "CASCADE", "RESTRICT")],
            triggers: vec![Trigger {
                name: "orders_before_update".into(),
                timing: "BEFORE".into(),
                event: "UPDATE".into(),
                definer: "app@%".into(),
                body: "BEGIN\n  IF NEW.status = 'refunded' AND OLD.status <> 'paid' THEN\n    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'Only paid orders can be refunded';\n  END IF;\nEND".into(),
            }],
            status: db::sql::structure::Status {
                engine: "InnoDB".into(),
                row_format: "Dynamic".into(),
                collation: "utf8mb4_0900_ai_ci".into(),
                rows: Some(4410),
                avg_row: Some(512),
                data: Some(2_202_009),
                index: Some(1_468_006),
                free: Some(4_194_304),
                auto_increment: Some(4411),
                created: "2025-02-11 09:12:40".into(),
                updated: "2026-11-09 12:41:07".into(),
                comment: "Customer orders".into(),
            },
            create: create.into(),
        };
        let Some(t) = self.tabs.first_mut() else { return };
        t.view = view;
        t.selected = None;
        if let Some(s) = t.sql.as_mut() {
            s.st.data = Some(d);
            s.st.sel = [Some(3), None, Some(0), None, Some(0)];
        }
    }
}
