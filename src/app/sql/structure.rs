//! The read-only SQL views about one table: Structure, Relations, Triggers and Info. They share one
//! fetch (`db::sql::Structure`), asked for when a view first draws and again on refresh. Columns and
//! indexes are edited here through staged changes (`sql_alter.rs`); the other three views stay read-only.

use crate::app::*;
use crate::ui::dnd;
use crate::app::sql::alter::staged_marks;
use db::sql::ddl::Change;
use db::sql::structure::{Column, ForeignKey, Index, Structure, Trigger};
use egui::{pos2, vec2, Rect};

const READ_ONLY: &str = "Read-only: unlock writes to change the structure";
const LATER: &str = "Comes with writes";

/// What the four views keep per tab.
#[derive(Default)]
pub(in crate::app) struct StructState {
    pub(in crate::app) load: Load,
    pub(in crate::app) data: Option<Structure>,
    /// The selected row of each grid: columns, indexes, keys out, keys in, triggers.
    pub(in crate::app) sel: [Option<usize>; 5],
    /// Column and index changes waiting to become one ALTER TABLE.
    pub(in crate::app) staged: Vec<Change>,
    /// The statement and warnings of `staged`, kept between frames.
    pub(in crate::app) bar: Option<crate::app::sql::alter::PlanMemo>,
    /// The ALTER or trigger statement is running.
    pub(in crate::app) applying: bool,
    /// Trigger edits not saved yet, by trigger name; the new trigger's is under the empty name.
    pub(in crate::app) trig: HashMap<String, db::sql::ddl::TrigDef>,
    /// The editor shows the new trigger instead of the selected one.
    pub(in crate::app) trig_new: bool,
    /// The name a trigger that is being made will have, so its draft is cleared when the structure lists it.
    pub(in crate::app) trig_made: Option<String>,
    /// The draft key of a trigger edit on its way, dropped once the server took it: the draft is the live trigger then,
    /// and left behind it would show an Undo or someone else's later change as a pending edit of ours.
    pub(in crate::app) trig_saving: Option<String>,
    /// What the trigger editor shows for the draft, kept between frames.
    pub(in crate::app) trig_memo: Option<sql::trig::TrigMemo>,
}

#[derive(Default)]
pub(in crate::app) enum Load {
    #[default]
    Idle,
    Loading,
    Failed(String),
}

impl StructState {
    pub(in crate::app) fn finish(&mut self, res: db::Res<Structure>) {
        match res {
            Ok(s) => (self.data, self.load) = (Some(s), Load::Idle),
            Err(e) => self.load = Load::Failed(e),
        }
    }
}

/// What a frame of drawing asks the app to do afterwards.
pub(in crate::app) enum Act {
    Copy(String),
    Open { db: String, table: String },
    Refresh,
    NewColumn,
    EditColumn(usize),
    DuplicateColumn(usize),
    DropColumn(usize),
    /// The column at the first index dragged to the place of the column at the second, in the order shown.
    MoveColumnTo(usize, usize),
    NewRelation,
    EditRelation(usize),
    DropRelation(usize),
    NewTrigger,
    DropTrigger(String),
    NewIndex,
    EditIndex(usize),
    DropIndex(usize),
    Unstage(usize),
    Discard,
    Apply,
}

impl App {
    /// Asks for the current tab's structure. The answer is matched back by `idx_req`.
    fn request_structure(&mut self) {
        let i = self.cur;
        let Some((db, space)) = self.tabs.get(i).and_then(|t| self.sql_target(&t.sel)) else { return };
        self.req += 1;
        let req = self.req;
        let t = &mut self.tabs[i];
        t.idx_req = req;
        if let Some(s) = t.sql.as_mut() {
            s.st.load = Load::Loading;
        }
        self.worker.sql_structure(req, db, space, t.sel.coll.clone());
    }

    /// The foreign keys of the open table, for the key cells of Content. The structure is asked for the first time
    /// they are wanted and kept until a change to it clears it.
    pub(in crate::app) fn sql_fks(&mut self) -> Vec<db::sql::structure::ForeignKey> {
        let Some(st) = self.tabs.get(self.cur).and_then(|t| t.sql.as_ref()).map(|s| &s.st) else { return Vec::new() };
        if st.data.is_none() && matches!(st.load, Load::Idle) && !self.demo {
            self.request_structure();
            return Vec::new();
        }
        self.tabs.get(self.cur).and_then(|t| t.sql.as_ref()).and_then(|s| s.st.data.as_ref()).map(|d| d.foreign_keys.clone()).unwrap_or_default()
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

    /// The staged changes and their kept plan, moved out of the tab for the frame so the draw reads them without a
    /// copy. `hold_staged` puts them back before the frame's actions run.
    fn take_staged(&mut self) -> (Vec<Change>, Option<crate::app::sql::alter::PlanMemo>, bool) {
        self.tabs[self.cur].sql.as_mut().map_or((Vec::new(), None, false), |s| (std::mem::take(&mut s.st.staged), s.st.bar.take(), s.st.applying))
    }

    fn hold_staged(&mut self, staged: Vec<Change>, memo: Option<crate::app::sql::alter::PlanMemo>) {
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            (s.st.staged, s.st.bar) = (staged, memo);
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
                let base = self.tabs[self.cur].sel.clone();
                // A link: without a per-tab Back, the table it was followed from stays in its tab.
                self.open_collection(sql::ns::with_ns(self.dialect_of(base.conn), &base, &db, table), Open::Link);
            }
            Act::Refresh => {
                if let Some(s) = self.tabs[self.cur].sql.as_mut() {
                    s.st.load = Load::Idle;
                }
                self.request_structure();
            }
            Act::NewColumn => self.open_col_form(None),
            Act::EditColumn(i) => self.open_col_form(Some(i)),
            Act::DuplicateColumn(i) => self.duplicate_column(i),
            Act::DropColumn(i) => self.stage_drop_column(i),
            Act::MoveColumnTo(i, to) => self.stage_move_column_to(i, to),
            Act::NewRelation => self.open_fk_form(None),
            Act::EditRelation(i) => self.open_fk_form(Some(i)),
            Act::DropRelation(i) => self.stage_drop_fk(i),
            Act::NewTrigger => self.trig_new(),
            Act::DropTrigger(n) => self.trig_ask_drop(n),
            Act::NewIndex => self.open_idx_form(None),
            Act::EditIndex(i) => self.open_idx_form(Some(i)),
            Act::DropIndex(i) => self.stage_drop_index(i),
            Act::Unstage(i) => self.unstage(i),
            Act::Discard => self.discard_staged(),
            Act::Apply => self.request_alter_apply(),
        }
    }

    /// The count shown beside a view's tab once the structure is known.
    pub(in crate::app) fn sql_view_count(&self, v: View) -> Option<usize> {
        let d = self.t()?.sql.as_ref()?.st.data.as_ref()?;
        Some(match v {
            View::Relations => d.foreign_keys.len(),
            View::Triggers => d.triggers.len(),
            _ => 0,
        })
        .filter(|n| *n > 0)
    }

    pub(in crate::app) fn sql_structure_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, mut sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        let editable = self.sel_writable();
        let is_view = self.sel_is_view();
        let why = if is_view { "Views cannot be changed here" } else { READ_ONLY };
        let note = if editable { "" } else { why };
        let (staged, mut memo, applying) = self.take_staged();
        let (dropped, changed) = staged_marks(&staged);
        let col = sel[0].filter(|i| *i < d.columns.len());
        let conn = self.t().map_or(0, |t| t.sel.conn);
        let pg = self.is_pg(conn);
        let (order, unsigned, collation, cardinality) = (self.supports(conn, db::sql::Feature::ColumnOrder), self.supports(conn, db::sql::Feature::Unsigned), self.supports(conn, db::sql::Feature::Collation), self.supports(conn, db::sql::Feature::IndexCardinality));
        let mut left = Vec::new();
        edit_row_with(ui, editable, note, |ui, on| {
            if quiet_enabled(ui, on, "+ Column").on_hover_text("Add a column").on_disabled_hover_text(why).clicked() {
                left.push(Act::NewColumn);
            }
            let hint = if on { "Select a column first" } else { why };
            let own = [("Edit", "Change the selected column", col.map(Act::EditColumn)), ("Duplicate", "Add a copy of the selected column", col.map(Act::DuplicateColumn)), ("Delete", "Drop the selected column", col.map(Act::DropColumn))];
            // Columns move by dragging their row, or Alt+Up and Alt+Down on the focused row.
            for (t, tip, act) in own {
                let r = quiet_enabled(ui, on && act.is_some(), t).on_hover_text(tip).on_disabled_hover_text(hint);
                if let (true, Some(a)) = (r.clicked(), act) {
                    left.push(a);
                }
            }
        }, |ui| {
            if quiet(ui, "Copy CREATE TABLE").clicked() {
                acts.push(Act::Copy(d.create.clone()));
            }
            if refresh_button(ui) {
                acts.push(Act::Refresh);
            }
        });
        acts.extend(left);
        ui.add_space(8.0);
        if !staged.is_empty() {
            self.staged_bar(ui, &mut memo, &d, &staged, editable, applying, &mut acts);
            ui.add_space(8.0);
        }
        egui::ScrollArea::vertical().id_salt("sql-structure").auto_shrink([false, false]).show(ui, |ui| {
            let shown: Vec<usize> = (0..10).filter(|i| (unsigned || *i != 3) && (collation || *i != 8)).collect();
            let all = [gc("Name", 1.5), gc("Type", 1.0), gc("Length / values", 1.4), gc("Unsigned", 0.95), gc("Null", 0.6), gc("Key", 0.6), gc("Default", 1.9), gc("Extra", 2.1), gc("Collation", 1.9), gc("Comment", 1.3)];
            let heads: Vec<GCol> = all.into_iter().enumerate().filter(|(i, _)| shown.contains(i)).map(|(_, g)| g).collect();
            let mut menu = |ui: &mut egui::Ui, r: usize| {
                for (label, act) in [("Edit column", Act::EditColumn(r)), ("Duplicate", Act::DuplicateColumn(r))] {
                    if ui.add_enabled(editable, egui::Button::new(label)).on_disabled_hover_text(why).clicked() {
                        acts.push(act);
                        ui.close();
                    }
                }
                ui.separator();
                ui.add_enabled_ui(editable, |ui| {
                    if danger_item(ui, "Drop column").clicked() {
                        acts.push(Act::DropColumn(r));
                        ui.close();
                    }
                })
                .response
                .on_disabled_hover_text(why);
                ui.separator();
                if ui.button("Copy name").clicked() {
                    acts.push(Act::Copy(d.columns[r].name.clone()));
                    ui.close();
                }
            };
            // The rows show in the order the staged changes leave, so a staged move shows at once and the next drag or
            // Alt+arrow starts from there. `at` maps a row to its column in `d.columns`.
            let at = rows_in_staged_order(&d, &staged);
            // Rows drag only where a move can be staged: a writable table on an engine that keeps column order.
            let keys: Vec<egui::Id> = at.iter().map(|&i| egui::Id::unique(("structure-col", d.columns[i].name.as_str()))).collect();
            let drag = (editable && order && !is_view).then_some(keys.as_slice());
            let hit = grid_drag(
                ui,
                "columns",
                &heads,
                at.len(),
                sel[0].and_then(|s| at.iter().position(|&i| i == s)),
                &mut |ui, rect, r, c| {
                    let r = at[r];
                    if c == 0 {
                        let name = &d.columns[r].name;
                        let mark = if dropped.contains(name) { Some(ALERT()) } else if changed.contains(name) { Some(WARNING()) } else { None };
                        if let Some(m) = mark {
                            ui.painter().rect_filled(Rect::from_min_size(pos2(rect.left() - 40.0, rect.top()), vec2(3.0, rect.height())), 0.0, m);
                        }
                    }
                    column_cell(ui, rect, &d.columns[r], shown[c], pg)
                },
                Some(&mut |ui: &mut egui::Ui, r: usize| menu(ui, at[r])),
                drag,
            );
            if let Some(r) = hit.click {
                sel[0] = Some(at[r]);
            }
            if let Some(m) = hit.moved {
                acts.push(Act::MoveColumnTo(at[m.from], at[m.to]));
                sel[0] = Some(at[m.from]);
            }
            if let (Some(r), true) = (hit.double, editable) {
                acts.push(Act::EditColumn(at[r]));
            }
            ui.add_space(14.0);
            let ix = sel[1].filter(|i| *i < d.indexes.len());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                caption(ui, "INDEXES");
                stat_chip(ui, &d.indexes.len().to_string());
                right_group(ui, |ui| {
                    if quiet_enabled(ui, editable, "+ Index").on_disabled_hover_text(why).clicked() {
                        acts.push(Act::NewIndex);
                    }
                    if let Some(i) = ix {
                        if quiet_enabled(ui, editable, "Drop").on_disabled_hover_text(why).clicked() {
                            acts.push(Act::DropIndex(i));
                        }
                        if quiet_enabled(ui, editable, "Edit").on_disabled_hover_text(why).clicked() {
                            acts.push(Act::EditIndex(i));
                        }
                    }
                });
            });
            ui.add_space(6.0);
            let ishown: Vec<usize> = (0..6).filter(|i| cardinality || *i != 4).collect();
            let iall = [gc("Key name", 2.0), gc("Columns", 2.4), gc("Unique", 0.8), gc("Type", 0.8), gc_r("Cardinality", 1.2), gc("Comment", 2.4)];
            let heads: Vec<GCol> = iall.into_iter().enumerate().filter(|(i, _)| ishown.contains(i)).map(|(_, g)| g).collect();
            let mut imenu = |ui: &mut egui::Ui, r: usize| {
                if ui.add_enabled(editable, egui::Button::new("Edit index")).on_disabled_hover_text(why).clicked() {
                    acts.push(Act::EditIndex(r));
                    ui.close();
                }
                ui.separator();
                ui.add_enabled_ui(editable, |ui| {
                    if danger_item(ui, "Drop index").clicked() {
                        acts.push(Act::DropIndex(r));
                        ui.close();
                    }
                })
                .response
                .on_disabled_hover_text(why);
            };
            let hit = grid_ex(ui, "indexes", &heads, d.indexes.len(), sel[1], |ui, rect, r, c| index_cell(ui, rect, &d.indexes[r], ishown[c]), Some(&mut imenu));
            if let Some(r) = hit.click {
                sel[1] = Some(r);
            }
            if let (Some(r), true) = (hit.double, editable) {
                acts.push(Act::EditIndex(r));
            }
        });
        self.hold_staged(staged, memo);
        self.keep(&ui.ctx().clone(), d, sel, acts);
    }

    pub(in crate::app) fn sql_relations_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, mut sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        let mut open = None;
        let editable = self.sel_writable();
        let why = if self.sel_is_view() { "Views cannot be changed here" } else { READ_ONLY };
        let (staged, mut memo, applying) = self.take_staged();
        let dropped: Vec<String> = staged.iter().filter_map(|c| if let Change::DropFk(n) = c { Some(n.to_ascii_lowercase()) } else { None }).collect();
        let fk = sel[2].filter(|i| *i < d.foreign_keys.len());
        // Changing a key is a drop and an add in one transaction, which only PostgreSQL does.
        let pg_rel = d.pg_schema.is_some();
        let mut left = Vec::new();
        edit_row_with(ui, editable, if editable { "" } else { why }, |ui, on| {
            if quiet_enabled(ui, on, "+ Relation").on_hover_text("Add a foreign key").on_disabled_hover_text(why).clicked() {
                left.push(Act::NewRelation);
            }
            let hint = if on { "Select a relation first" } else { why };
            if pg_rel {
                let r = quiet_enabled(ui, on && fk.is_some(), "Edit").on_hover_text("Change the selected foreign key").on_disabled_hover_text(hint);
                if let (true, Some(i)) = (r.clicked(), fk) {
                    left.push(Act::EditRelation(i));
                }
            }
            let r = quiet_enabled(ui, on && fk.is_some(), "Delete").on_hover_text("Drop the selected foreign key").on_disabled_hover_text(hint);
            if let (true, Some(i)) = (r.clicked(), fk) {
                left.push(Act::DropRelation(i));
            }
        }, |ui| {
            if refresh_button(ui) {
                acts.push(Act::Refresh);
            }
            ui.label(RichText::new("Click a foreign key cell in Content to open the row it points to").size(TXT_SMALL).color(FAINT()));
        });
        acts.extend(left);
        ui.add_space(8.0);
        if !staged.is_empty() {
            self.staged_bar(ui, &mut memo, &d, &staged, editable, applying, &mut acts);
            ui.add_space(8.0);
        }
        // The schema's estimate, asked for once per database; a table it has not listed yet shows nothing.
        let (conn, base_db) = (self.tabs[self.cur].sel.conn, self.tabs[self.cur].sel.db.clone());
        let dialect = self.dialect_of(conn);
        let dbs: std::collections::BTreeSet<String> = d.referenced_by.iter().map(|fk| fk.db.clone()).collect();
        for ns in dbs {
            let (db, schema) = sql::ns::split_ns(dialect, &base_db, &ns);
            self.sql_serve(conn, &db, &schema, crate::ui::sql_complete::Wants { schema: true, values: None });
        }
        let in_rows: Vec<Option<u64>> = d.referenced_by.iter().map(|fk| { let (db, schema) = sql::ns::split_ns(dialect, &base_db, &fk.db); self.sql_schema.get(&(conn, ns_key(&db, &schema))).and_then(|s| s.table(&fk.table)).and_then(|t| t.rows) }).collect();
        egui::ScrollArea::vertical().id_salt("sql-relations").auto_shrink([false, false]).show(ui, |ui| {
            caption(ui, "THIS TABLE POINTS TO");
            ui.add_space(6.0);
            let heads = [gc("Name", 2.0), gc("Column", 1.6), gc("References", 2.0), gc("On update", 1.2), gc("On delete", 1.2), gc("Index", 2.0)];
            let mut menu = |ui: &mut egui::Ui, r: usize| {
                if pg_rel {
                    ui.add_enabled_ui(editable, |ui| {
                        if ui.button("Edit relation").clicked() {
                            acts.push(Act::EditRelation(r));
                            ui.close();
                        }
                    })
                    .response
                    .on_disabled_hover_text(why);
                }
                ui.add_enabled_ui(editable, |ui| {
                    if danger_item(ui, "Drop relation").clicked() {
                        acts.push(Act::DropRelation(r));
                        ui.close();
                    }
                })
                .response
                .on_disabled_hover_text(why);
                ui.separator();
                if ui.button("Copy name").clicked() {
                    acts.push(Act::Copy(d.foreign_keys[r].name.clone()));
                    ui.close();
                }
            };
            let hit = grid_ex(ui, "fk-out", &heads, d.foreign_keys.len(), sel[2], |ui, rect, r, c| {
                let fk = &d.foreign_keys[r];
                match c {
                    0 => {
                        if dropped.contains(&fk.name.to_ascii_lowercase()) {
                            ui.painter().rect_filled(Rect::from_min_size(pos2(rect.left() - 40.0, rect.top()), vec2(3.0, rect.height())), 0.0, ALERT());
                        }
                        put(ui, rect, &fk.name, mono(TXT_SMALL), TEXT(), false);
                    }
                    1 => put(ui, rect, &fk.cols.join(", "), mono(TXT_SMALL), TEXT(), false),
                    2 => {
                        let other = if fk.ref_db == fk.db { String::new() } else { format!("{}.", fk.ref_db) };
                        if link(ui, rect, ("fk-out-link", r), &format!("{other}{}.{}", fk.ref_table, fk.ref_cols.join(", "))) {
                            open = Some((fk.ref_db.clone(), fk.ref_table.clone()));
                        }
                    }
                    3 => put(ui, rect, &fk.on_update, mono(TXT_SMALL), C_KEYWORD(), false),
                    4 => put(ui, rect, &fk.on_delete, mono(TXT_SMALL), C_KEYWORD(), false),
                    _ => put(ui, rect, d.index_for(fk).map_or("", |i| i.name.as_str()), mono(TXT_SMALL), MUTED(), false),
                }
            }, Some(&mut menu));
            if let Some(r) = hit.click {
                sel[2] = Some(r);
            }
            if let (Some(r), true, true) = (hit.double, editable, pg_rel) {
                acts.push(Act::EditRelation(r));
            }
            ui.add_space(16.0);
            caption(ui, "TABLES THAT POINT HERE");
            ui.add_space(6.0);
            let heads = [gc("Table", 2.0), gc("Column", 1.6), gc("Constraint", 2.0), gc("On update", 1.2), gc("On delete", 1.2), gc("Rows", 1.2)];
            if let Some(r) = grid(ui, "fk-in", &heads, d.referenced_by.len(), sel[3], |ui, rect, r, c| {
                let fk = &d.referenced_by[r];
                match c {
                    0 => {
                        let other = if fk.db == fk.ref_db { String::new() } else { format!("{}.", fk.db) };
                        if link(ui, rect, ("fk-in-link", r), &format!("{other}{}", fk.table)) {
                            open = Some((fk.db.clone(), fk.table.clone()));
                        }
                    }
                    1 => put(ui, rect, &fk.cols.join(", "), mono(TXT_SMALL), TEXT(), false),
                    2 => put(ui, rect, &fk.name, mono(TXT_SMALL), MUTED(), false),
                    3 => put(ui, rect, &fk.on_update, mono(TXT_SMALL), C_KEYWORD(), false),
                    4 => put(ui, rect, &fk.on_delete, mono(TXT_SMALL), C_KEYWORD(), false),
                    _ => put(ui, rect, &in_rows[r].map_or(String::new(), |n| group(&n.to_string())), mono(TXT_SMALL), C_NUMBER(), false),
                }
            }) {
                sel[3] = Some(r);
            }
        });
        acts.extend(open.map(|(db, table)| Act::Open { db, table }));
        self.hold_staged(staged, memo);
        self.keep(&ui.ctx().clone(), d, sel, acts);
    }

    pub(in crate::app) fn sql_triggers_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, mut sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        // A locked connection still edits: Save and Delete ask to unlock, as Apply does for columns.
        let editable = self.trig_editable();
        let why = "Views cannot be changed here";
        // A trigger that was just created is listed now, so its draft has done its job.
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            let made = s.st.trig_made.clone();
            if let Some(i) = s.st.trig.get(sql::trig::NEW).and_then(|nd| d.triggers.iter().position(|t| (t.name == nd.name && sql::trig::same(&sql::trig::def_of(t), nd)) || made.as_deref() == Some(t.name.as_str()))) {
                s.st.trig.remove(sql::trig::NEW);
                (s.st.trig_new, sel[4], s.st.trig_made) = (false, Some(i), None);
            }
            sql::trig::prune_drafts(&mut s.st.trig, &d.triggers);
        }
        let creating = self.tabs[self.cur].sql.as_ref().is_some_and(|s| s.st.trig_new);
        let no_drafts = HashMap::new();
        let drafts = self.tabs[self.cur].sql.as_ref().map_or(&no_drafts, |s| &s.st.trig);
        let key = self.trig_key(&d, sel[4]);
        let existing = key.as_ref().filter(|k| !k.is_empty()).cloned();
        let mut left = Vec::new();
        edit_row_with(ui, editable, if editable { "" } else { why }, |ui, on| {
            if quiet_enabled(ui, on, "+ Trigger").on_hover_text("Write a new trigger").on_disabled_hover_text(why).clicked() {
                left.push(Act::NewTrigger);
            }
            let hint = if on { "Select a trigger first" } else { why };
            let r = quiet_enabled(ui, on && existing.is_some(), "Delete").on_hover_text("Drop the selected trigger").on_disabled_hover_text(hint);
            if let (true, Some(n)) = (r.clicked(), existing.clone()) {
                left.push(Act::DropTrigger(n));
            }
        }, |ui| {
            if refresh_button(ui) {
                acts.push(Act::Refresh);
            }
            if let Some(t) = existing.as_ref().and_then(|n| d.triggers.iter().find(|t| &t.name == n)) && quiet(ui, "Copy CREATE").clicked() {
                acts.push(Act::Copy(t.create_sql(&self.tabs[self.cur].sel.coll)));
            }
        });
        acts.extend(left);
        ui.add_space(8.0);
        let heads = [gc("Name", 2.0), gc("Timing", 0.9), gc("Event", 0.9), gc("Definer", 1.5), gc("Statement", 4.0)];
        let hl = if creating { None } else { sel[4] };
        if let Some(r) = grid(ui, "triggers", &heads, d.triggers.len(), hl, |ui, rect, r, c| {
            let t = &d.triggers[r];
            match c {
                0 => {
                    let edited = drafts.get(&t.name).is_some_and(|x| !sql::trig::same(&sql::trig::def_of(t), x));
                    put(ui, rect, &t.name, mono(TXT_SMALL), if edited { WARNING() } else { TEXT() }, false)
                }
                1 => put(ui, rect, &t.timing, mono(TXT_SMALL), C_KEYWORD(), false),
                2 => put(ui, rect, &t.event, mono(TXT_SMALL), C_KEYWORD(), false),
                3 => put(ui, rect, &t.definer, mono(TXT_SMALL), MUTED(), false),
                _ => put(ui, rect, &preview(t), mono(TXT_SMALL), FAINT(), false),
            }
        }) {
            sel[4] = Some(r);
            if let Some(s) = self.tabs[self.cur].sql.as_mut() {
                s.st.trig_new = false;
            }
        }
        ui.add_space(12.0);
        match &key {
            Some(k) => self.trig_editor(ui, &d, k, editable),
            None => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("This table has no triggers").size(TXT_SMALL).color(FAINT())));
            }
        }
        self.keep(&ui.ctx().clone(), d, sel, acts);
    }

    pub(in crate::app) fn sql_info_view(&mut self, ui: &mut egui::Ui) {
        let Some((d, sel)) = self.take_structure(ui) else { return };
        let mut acts = Vec::new();
        let s = &d.status;
        let none = || "-".to_string();
        let bytes = |n: Option<u64>| n.map_or_else(none, size);
        let pg = self.is_pg(self.tabs[self.cur].sel.conn);
        let rows: Vec<(&str, String)> = if pg {
            // PostgreSQL has no engine, row format or auto increment to report: the sizes come from the catalog.
            vec![
                ("Rows (estimate)", s.rows.map_or_else(none, |n| format!("~{}", group(&n.to_string())))),
                ("Data", bytes(s.data)),
                ("Indexes", bytes(s.index)),
                ("Total", bytes(s.total)),
                ("Comment", s.comment.clone()),
            ]
        } else {
            vec![
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
            ]
        };
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
                if !pg {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        for name in ["Analyze", "Optimize", "Check"] {
                            ui.add_enabled_ui(false, |ui| ui.button(name)).response.on_disabled_hover_text(LATER);
                        }
                        ui.add_enabled_ui(false, |ui| danger(ui, "Truncate...")).response.on_disabled_hover_text(LATER);
                    });
                }
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

/// The Structure view's row: `buttons` draws the actions given whether writes are allowed, then the note.
fn edit_row_with(ui: &mut egui::Ui, on: bool, note: &str, buttons: impl FnOnce(&mut egui::Ui, bool), right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        buttons(ui, on);
        ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
        right_group(ui, right);
    });
}

/// "1.4 MB" for a byte count, one decimal from kilobytes up.
pub(in crate::app) fn size(n: u64) -> String {
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
    place(ui, cell, s, font, color, right, 8.0);
}

/// A name that opens another table. True when clicked.
fn link(ui: &mut egui::Ui, cell: Rect, id: (&str, usize), s: &str) -> bool {
    let area = cell.shrink2(vec2(8.0, 0.0));
    let g = ui.painter().layout_no_wrap(s.to_string(), FontId::monospace(TXT_SMALL), C_LINK());
    let hit = Rect::from_min_size(pos2(area.left(), cell.top() + 4.0), vec2(g.size().x.min(area.width()), cell.height() - 8.0));
    let r = ui.interact(hit, ui.scope_id().with(id), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let ink = place(ui, cell, s, FontId::monospace(TXT_SMALL), C_LINK(), false, 8.0);
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

/// What a PostgreSQL column's Extra says: its catalog text names MySQL's auto_increment and the full GENERATED clause, which
/// the Structure table shortens to the two things a reader looks for.
fn pg_extra(extra: &str) -> String {
    if extra.contains("GENERATED ALWAYS AS IDENTITY") {
        "identity (ALWAYS)".into()
    } else if extra.contains("IDENTITY BY DEFAULT") {
        "identity (BY DEFAULT)".into()
    } else if extra.starts_with("GENERATED ALWAYS AS (") {
        "generated".into()
    } else if extra == "auto_increment" {
        "serial".into()
    } else {
        extra.to_string()
    }
}

fn column_cell(ui: &egui::Ui, rect: Rect, c: &Column, col: usize, pg: bool) {
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
            // PostgreSQL types carry the kind its catalog gave them, which sees through domains and enums.
            Some(d) if c.kind.map_or_else(|| matches!(base.as_str(), "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" | "DECIMAL" | "NUMERIC" | "FLOAT" | "DOUBLE" | "BIT" | "YEAR"), crate::app::sql::edit::is_numeric) => {
                put(ui, rect, d, mono(), C_NUMBER(), false);
            }
            Some(d) => {
                put(ui, rect, &format!("'{d}'"), mono(), C_STRING(), false);
            }
        },
        7 => {
            let extra = if pg { pg_extra(&c.extra) } else { c.extra.replace("DEFAULT_GENERATED", "") };
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
fn grid(ui: &mut egui::Ui, salt: &str, cols: &[GCol], rows: usize, sel: Option<usize>, cell: impl FnMut(&mut egui::Ui, Rect, usize, usize)) -> Option<usize> {
    grid_ex(ui, salt, cols, rows, sel, cell, None).click
}

/// What a frame of a grid saw: the row clicked (a right click counts, so a menu acts on what it opened over),
/// the row double-clicked and, for a grid whose rows drag, a row let go somewhere else.
#[derive(Default)]
struct Hit {
    click: Option<usize>,
    double: Option<usize>,
    moved: Option<dnd::Move>,
}

/// `grid` with a double-click and a row menu, which gets the row it opened on. With `drag` the rows drag to reorder and
/// Alt+Up or Alt+Down moves the focused one; `keys` are the rows' stable keys.
#[allow(clippy::too_many_arguments)]
fn grid_ex(ui: &mut egui::Ui, salt: &str, cols: &[GCol], rows: usize, sel: Option<usize>, mut cell: impl FnMut(&mut egui::Ui, Rect, usize, usize), menu: Option<&mut dyn FnMut(&mut egui::Ui, usize)>) -> Hit {
    grid_drag(ui, salt, cols, rows, sel, &mut cell, menu, None)
}

#[allow(clippy::too_many_arguments)]
fn grid_drag(ui: &mut egui::Ui, salt: &str, cols: &[GCol], rows: usize, sel: Option<usize>, cell: &mut dyn FnMut(&mut egui::Ui, Rect, usize, usize), mut menu: Option<&mut dyn FnMut(&mut egui::Ui, usize)>, drag: Option<&[egui::Id]>) -> Hit {
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
    let keyed = drag.is_some();
    let mut hit = Hit::default();
    let mut draw_row = |ui: &mut egui::Ui, row: Rect, r: usize, st: dnd::ItemState, hit: &mut Hit| -> egui::Response {
        // A dragged grid draws each row in a scope keyed by the row's own key, so the id holds still when the row moves and
        // it keeps its focus for the next Alt+arrow.
        let resp = ui.interact(row, ui.scope_id().with(("sql-grid", salt, if keyed { 0 } else { r })), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, sel == Some(r), format!("{salt} row {}", r + 1)));
        let hovered = resp.hovered() && !st.busy;
        let fill = if sel == Some(r) { if hovered { SEL_HOVER() } else { SEL() } } else if hovered { HOVER() } else if st.lifted { RAISED() } else if r % 2 == 1 { stripe } else { BG() };
        ui.painter().rect_filled(row, 0.0, fill);
        put(ui, Rect::from_min_size(row.min, vec2(NUM, ROW_H)), &(r + 1).to_string(), FontId::monospace(TXT_CAPTION), FAINT(), false);
        for (i, (a, b)) in xs.iter().enumerate() {
            cell(ui, Rect::from_min_max(pos2(*a, row.top()), pos2(*b, row.bottom())), r, i);
        }
        if st.lifted {
            return resp;
        }
        if resp.clicked() || resp.secondary_clicked() {
            hit.click = Some(r);
        }
        if resp.double_clicked() {
            hit.double = Some(r);
        }
        // A grid whose rows drag also moves them from the row menu, for a person who cannot drag or does not know Alt+arrows.
        let can_move = keyed && !st.busy;
        if menu.is_some() || can_move {
            resp.context_menu(|ui| {
                menu_look(ui);
                if let Some(m) = menu.as_mut() {
                    m(ui, r);
                    if can_move {
                        ui.separator();
                    }
                }
                if can_move && let Some(mv) = dnd::move_items(ui, rows, r, &[], false) {
                    hit.moved = Some(mv);
                }
            });
        }
        resp
    };
    let body = Rect::from_min_size(pos2(rect.left(), rect.top() + TABLE_HEAD_H), vec2(w, rows as f32 * ROW_H));
    match drag {
        Some(keys) => {
            let sizes = vec![ROW_H; rows];
            let moved = put_at(ui, body, |ui| {
                dnd::reorder(ui, egui::Id::unique(("sql-grid-reorder", salt)), dnd::Items::vertical(&sizes).keys(keys), |ui, r, st| {
                    // The x of every cell is the grid's, so a row sliding to open the gap moves only along y.
                    let row = Rect::from_min_size(pos2(rect.left(), ui.max_rect().top()), vec2(w, ROW_H));
                    draw_row(ui, row, r, st, &mut hit)
                })
            });
            hit.moved = moved.or(hit.moved);
        }
        None => {
            for r in 0..rows {
                let row = Rect::from_min_size(pos2(rect.left(), body.top() + r as f32 * ROW_H), vec2(w, ROW_H));
                draw_row(ui, row, r, dnd::ItemState::default(), &mut hit);
            }
        }
    }
    ui.painter().hline(rect.x_range(), rect.top() + TABLE_HEAD_H, Stroke::new(1.0, LINE()));
    ui.painter().rect_stroke(rect, R_MD, Stroke::new(1.0, LINE()), egui::StrokeKind::Inside);
    hit
}

/// The column grid's rows as indexes into `d.columns`, in the order the staged changes leave the columns. A column
/// staged to be dropped keeps its place in the server's order, so its mark shows where it is.
fn rows_in_staged_order(d: &Structure, staged: &[Change]) -> Vec<usize> {
    let after = db::sql::ddl::columns_after(d, staged);
    // The name each column ends with, following its renames in order, or None once it is dropped. Matching on the last
    // rename alone, or on the server name, gave a staged name to a dropped column that had it, so the live column's row
    // went to the dropped one's place.
    let final_of = |c: &Column| -> Option<String> {
        let mut n = c.name.clone();
        for ch in staged {
            match ch {
                Change::Drop(x) if *x == n => return None,
                Change::Modify { from, def, .. } if *from == n => n = def.name.clone(),
                _ => {}
            }
        }
        Some(n)
    };
    let finals: Vec<Option<String>> = d.columns.iter().map(final_of).collect();
    let mut out: Vec<usize> = after.iter().filter_map(|n| finals.iter().position(|f| f.as_deref() == Some(n.as_str()))).collect();
    for i in 0..d.columns.len() {
        if !out.contains(&i) {
            out.insert(i.min(out.len()), i);
        }
    }
    out
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
    pub(in crate::app) fn sql_structure_demo(&mut self, which: &str) {
        let view = match which {
            "structure" | "coledit" | "stagebar" => View::Structure,
            "fkcell" => View::Documents,
            "info" => View::Info,
            "relations" | "addrel" => View::Relations,
            "triggers" | "trigedit" => View::Triggers,
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
            ..Default::default()
        };
        let ix = |name: &str, unique: bool, cols: &[(&str, bool)], card: u64, comment: &str| Index {
            name: name.into(),
            unique,
            cols: cols.iter().map(|(n, d)| db::sql::structure::IndexCol { name: (*n).into(), desc: *d, ..Default::default() }).collect(),
            kind: "BTREE".into(),
            cardinality: Some(card),
            comment: comment.into(),
            ..Default::default()
        };
        let fk = |name: &str, table: &str, c: &str, rt: &str, rc: &str, upd: &str, del: &str| ForeignKey { name: name.into(), db: "shop".into(), table: table.into(), cols: vec![c.into()], ref_db: "shop".into(), ref_table: rt.into(), ref_cols: vec![rc.into()], on_update: upd.into(), on_delete: del.into(), ..Default::default() };
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
                order: 1,
                sql_mode: "STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION".into(),
                body: "BEGIN\n  IF NEW.status = 'refunded' AND OLD.status <> 'paid' THEN\n    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'Only paid orders can be refunded';\n  END IF;\nEND".into(),
                pg: None,
            }, Trigger {
                name: "orders_after_insert".into(),
                timing: "AFTER".into(),
                event: "INSERT".into(),
                definer: "app@%".into(),
                order: 1,
                sql_mode: "STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION".into(),
                body: "INSERT INTO audit_log (tbl, row_id, action) VALUES ('orders', NEW.id, 'insert')".into(),
                pg: None,
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
                ..Default::default()
            },
            create: create.into(),
            ..Default::default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_renamed_to_a_dropped_name_keeps_its_own_row() {
        use db::sql::ddl::{ColDef, Pos};
        let col = |n: &str| Column { name: n.into(), ..Default::default() };
        let d = Structure { columns: vec![col("c"), col("a"), col("b")], ..Default::default() };
        // c is dropped, then a takes its name and moves after b.
        let staged = vec![Change::Drop("c".into()), Change::Modify { from: "a".into(), def: ColDef::new("c", "INT"), pos: Pos::After("b".into()) }];
        assert_eq!(db::sql::ddl::columns_after(&d, &staged), ["b", "c"]);
        // b, then a (now c), with the dropped c kept at its server place.
        assert_eq!(rows_in_staged_order(&d, &staged), [0, 2, 1]);
        // A plain rename and a drop, and no staged change.
        let staged = vec![Change::Modify { from: "b".into(), def: ColDef::new("bb", "INT"), pos: Pos::Keep }, Change::Drop("a".into())];
        assert_eq!(rows_in_staged_order(&d, &staged), [0, 1, 2]);
        assert_eq!(rows_in_staged_order(&d, &[]), [0, 1, 2]);
        let mut seen = rows_in_staged_order(&d, &staged);
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 3, "a column shown twice");
    }

    #[test]
    fn postgres_extra_names_identity_and_generated_columns() {
        assert_eq!(pg_extra("auto_increment GENERATED ALWAYS AS IDENTITY"), "identity (ALWAYS)");
        assert_eq!(pg_extra("auto_increment IDENTITY BY DEFAULT"), "identity (BY DEFAULT)");
        assert_eq!(pg_extra("GENERATED ALWAYS AS (id * 2) STORED"), "generated");
        assert_eq!(pg_extra("auto_increment"), "serial");
        assert_eq!(pg_extra(""), "");
    }
}
