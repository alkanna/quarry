//! Update matching and Delete all matching rows of the SQL Content view: the dialog that counts the rows the last page's
//! WHERE matches, shows the statement, takes the values of an update and asks for the table name before anything runs.

use crate::app::*;
use db::sql::bulk::{self, BulkOp, BulkPlan, BulkPrep};
use db::sql::value::{self, Kind};
use db::sql::Col;

/// One `column = value` of an update. `col` is the column's `Col::name`.
#[derive(Clone, Default)]
pub(in crate::app) struct Pair {
    pub(in crate::app) col: String,
    pub(in crate::app) text: String,
    pub(in crate::app) null: bool,
}

pub(in crate::app) struct BulkForm {
    pub(in crate::app) update: bool,
    pub(in crate::app) sel: Sel,
    /// The namespace the page was read in, which the count and the write use too.
    pub(in crate::app) ns: String,
    /// The WHERE of the last page asked for, kept from when the dialog opened: the grid may reload while it is open.
    pub(in crate::app) filter: String,
    /// The last page had a LIMIT or OFFSET chip, which the write does not follow.
    pub(in crate::app) capped: bool,
    pub(in crate::app) req: u64,
    pub(in crate::app) prep: Option<BulkPrep>,
    pub(in crate::app) pairs: Vec<Pair>,
    pub(in crate::app) typed: String,
}

/// Columns an update may set: the key would break undo, and computed and geometry columns cannot take a typed value.
fn settable<'a>(p: &'a BulkPrep) -> Vec<&'a Col> {
    p.cols.iter().filter(|c| !p.pk.contains(&c.src) && !c.generated && c.kind != Kind::Geometry).collect()
}

/// The value of one pair, read as the grid's cell editor reads it.
fn pair_value(d: db::sql::Dialect, col: &Col, p: &Pair) -> Result<Bson, String> {
    if p.null {
        Ok(Bson::Null)
    } else if col.kind == Kind::Enum {
        Ok(Bson::String(p.text.clone()))
    } else {
        value::parse_col(d, col, &p.text)
    }
}

/// The plan the dialog would run, or why it cannot yet.
fn plan_of(f: &BulkForm, d: db::sql::Dialect, bs: bool) -> Result<BulkPlan, String> {
    let Some(p) = &f.prep else { return Err("Counting…".into()) };
    let op = if f.update {
        let mut sets = Vec::new();
        for pair in &f.pairs {
            let col = p.cols.iter().find(|c| c.name == pair.col).ok_or_else(|| format!("{} is not a column of this table", pair.col))?;
            if sets.iter().any(|(n, _): &(String, Bson)| *n == col.name) {
                return Err(format!("{} is set twice", col.name));
            }
            sets.push((col.name.clone(), pair_value(d, col, pair).map_err(|m| format!("{}: {m}", col.name))?));
        }
        BulkOp::Update(sets)
    } else {
        BulkOp::Delete
    };
    Ok(BulkPlan { op, filter: f.filter.clone(), cols: p.cols.clone(), pk: p.pk.clone(), bs, expected: p.count, undo: bulk::undo_fits(p.count), engine: p.engine.clone() })
}

impl App {
    /// The More actions menu's Update matching and Delete all matching rows. They act on the WHERE of the page on screen.
    pub(in crate::app) fn open_bulk(&mut self, update: bool) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref() else { return };
        if let Some(why) = self.sql_edit_block() {
            return self.warn(why);
        }
        if s.ran.grouped() {
            return self.warn("Grouped rows are summaries, not table rows. Ungroup to change rows.");
        }
        if !self.sel_writable() {
            return self.ask_unlock_then(move |a| a.open_bulk(update));
        }
        let (sel, ran) = (t.sel.clone(), s.ran.clone());
        if self.sql_db_for(sel.conn, Some("Connect first.")).is_none() {
            return;
        }
        let Some((db, ns)) = self.sql_target(&sel) else { return self.warn_offline() };
        self.req += 1;
        let req = self.req;
        self.worker.sql_bulk_prep(req, db, ns.clone(), sel.coll.clone(), ran.filter.clone());
        let capped = ran.limit.is_some() || ran.skip > 0;
        self.modal = Some(Modal::SqlBulk(Box::new(BulkForm { update, sel, ns, filter: ran.filter, capped, req, prep: None, pairs: Vec::new(), typed: String::new() })));
    }

    /// The count and columns are back. A failure closes the dialog and says why.
    pub(in crate::app) fn on_bulk_prep(&mut self, req: u64, res: db::Res<BulkPrep>) {
        let Some(Modal::SqlBulk(f)) = &mut self.modal else { return };
        if f.req != req {
            return;
        }
        match res {
            Ok(p) => {
                if f.update && f.pairs.is_empty() && let Some(c) = settable(&p).first() {
                    f.pairs.push(Pair { col: c.name.clone(), ..Default::default() });
                }
                f.prep = Some(p);
            }
            Err(e) => {
                self.modal = None;
                self.err(e);
            }
        }
    }

    pub(in crate::app) fn sql_bulk_dialog(&mut self, ui: &mut egui::Ui, f: &mut BulkForm, keep: &mut bool) {
        let d = self.dialect_of(f.sel.conn);
        let bs = self.conn(f.sel.conn).and_then(|c| c.sql.as_ref()).is_none_or(|x| x.backslash_escapes());
        let table = f.sel.coll.clone();
        ui.set_width(600.0);
        let title = if f.update { format!("Update matching rows of {table}") } else { format!("Delete matching rows of {table}") };
        dialog_head(ui, &Head::new(title).sub(format!("{}  ·  {}", f.sel.ns(), self.conn_name(f.sel.conn))).glyph(if f.update { glyphs::pencil } else { glyphs::trash }).tone(ALERT()));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        if f.filter.trim().is_empty() {
            banner(ui, Notice::Danger, "No WHERE.", &format!("This {} every row of {table}.", if f.update { "changes" } else { "deletes" }), None);
        }
        if f.capped {
            banner(ui, Notice::Warn, "", "LIMIT and OFFSET only cap what the grid shows. Every row the WHERE matches is changed.", None);
        }
        if let Some(p) = f.prep.as_ref().filter(|p| !p.rolls_back()) {
            banner(ui, Notice::Warn, "No rollback.", &format!("The {} engine of {table} has no transactions: if the rows change before the write, what it changed stays.", p.engine), None);
        }
        let count = f.prep.as_ref().map(|p| p.count);
        ui.horizontal(|ui| match count {
            None => {
                spin(ui);
                ui.label(RichText::new("Counting the matching rows…").color(MUTED()));
            }
            Some(n) => {
                let what = if n == 1 { "1 row matches".to_string() } else { format!("{} rows match", group(&n.to_string())) };
                ui.label(semibold(RichText::new(what).size(TXT_TITLE).color(if n == 0 { MUTED() } else { BRIGHT() })));
            }
        });
        ui.add_space(10.0);
        if f.update && let Some(p) = f.prep.clone() {
            let cols = settable(&p);
            card_caption(ui, "Set", "NULL is offered for columns that allow it");
            let mut remove = None;
            let many = f.pairs.len() > 1;
            plain_card(ui, |ui| {
                for (i, pair) in f.pairs.iter_mut().enumerate() {
                    let Some(col) = cols.iter().find(|c| c.name == pair.col).copied() else { continue };
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("bulk-col", i)).width(190.0).selected_text(format!("{}  {}", col.name, col.ty.to_ascii_uppercase())).show_ui(ui, |ui| {
                            menu_look(ui);
                            for c in &cols {
                                if ui.selectable_label(c.name == col.name, format!("{}  {}", c.name, c.ty.to_ascii_uppercase())).clicked() {
                                    *pair = Pair { col: c.name.clone(), ..Default::default() };
                                }
                            }
                        });
                        ui.label(RichText::new("=").monospace().color(MUTED()));
                        let opts = db::sql::schema::enum_values(&col.ty);
                        if col.kind == Kind::Enum && !opts.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.set_max_width(260.0);
                                for o in &opts {
                                    if chip(ui, &format!("'{o}'"), PRIMARY(), !pair.null && pair.text == *o).clicked() {
                                        (pair.text, pair.null) = (o.clone(), false);
                                    }
                                }
                            });
                        } else {
                            let name = format!("Value of {}", col.name);
                            ui.add_enabled(!pair.null, egui::TextEdit::singleline(&mut pair.text).font(egui::TextStyle::Monospace).desired_width(240.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, !pair.null, &name));
                        }
                        if col.nullable {
                            ui.checkbox(&mut pair.null, "NULL");
                        }
                        if many && quiet(ui, "Remove").on_hover_text("Leave this column as it is").clicked() {
                            remove = Some(i);
                        }
                    });
                    if !pair.null && col.kind != Kind::Enum && let Err(m) = value::parse_col(d, col, &pair.text) {
                        field_note(ui, &m, ALERT());
                    }
                }
                let unused = cols.iter().find(|c| !f.pairs.iter().any(|p| p.col == c.name));
                if let Some(c) = unused && quiet(ui, "+ Column").on_hover_text("Set another column").clicked() {
                    f.pairs.push(Pair { col: c.name.clone(), ..Default::default() });
                }
            });
            if let Some(i) = remove {
                f.pairs.remove(i);
            }
            ui.add_space(10.0);
        }
        let plan = plan_of(f, d, bs);
        let stmt = plan.as_ref().map_err(String::clone).and_then(|p| bulk::statement(d, &f.ns, &table, p));
        card_caption(ui, "Statement", "runs as it reads here, in one transaction");
        well_frame().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().id_salt("bulk-stmt").max_height(200.0).auto_shrink([false, true]).show(ui, |ui| match &stmt {
                Ok(s) => {
                    ui.add(egui::Label::new(sql_job_in(&format!("{s};"), TXT_SMALL, d)).selectable(true));
                }
                Err(m) => {
                    ui.label(RichText::new(m).size(TXT_SMALL).color(MUTED()));
                }
            });
        });
        ui.add_space(10.0);
        if let Some(n) = count.filter(|n| *n > 0) {
            if bulk::undo_fits(n) {
                banner(ui, Notice::Info, "", &format!("Undo is available: the old rows are kept until you undo or quit, up to {} rows.", group(&db::sql::import::UNDO_MAX_ROWS.to_string())), None);
            } else {
                banner(ui, Notice::Danger, "No undo.", &format!("Over {} rows: the old rows are not kept, so this cannot be undone.", group(&db::sql::import::UNDO_MAX_ROWS.to_string())), None);
            }
        }
        crate::app::modals::typed_confirm(ui, &table, &mut f.typed);
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
            let n = count.unwrap_or(0);
            let rows = if n == 1 { "1 row".to_string() } else { format!("{} rows", group(&n.to_string())) };
            let label = if f.update { format!("Update {rows}") } else { format!("Delete {rows}") };
            let ready = n > 0 && stmt.is_ok() && f.typed == table;
            if danger_solid(ui, ready, &label).clicked() && let Ok(p) = plan.clone() {
                *keep = false;
                self.run_bulk(f, p);
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    /// Sends a confirmed plan. A connection locked again since the dialog opened is refused here and by the worker.
    fn run_bulk(&mut self, f: &BulkForm, plan: BulkPlan) {
        let conn = f.sel.conn;
        if !self.conn_writable(conn) {
            return self.warn_ro();
        }
        if self.sql_db_for(conn, Some("Connect first.")).is_none() {
            return;
        }
        let Some((db, _)) = self.sql_target(&f.sel) else { return self.warn_offline() };
        self.worker.sql_bulk(conn, db, f.ns.clone(), f.sel.coll.clone(), plan, !self.conn_writable(conn));
    }
}
