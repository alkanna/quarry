//! Functions and procedures of a SQL database: the ROUTINES group of the sidebar and the window that opens one to read, edit,
//! run or drop it. The statements and their checks are in `db::sql::routines`; this file draws, asks to unlock where a change
//! needs it, and keeps what the window has read. A routine opens in a window, not a tab, so it does not compete with the table
//! tabs for the page and stays beside the table that calls it.

use crate::app::*;
use db::sql::routines::{self as rt, Arg, ArgMode, Detail, Kind, Plan, Routine};
use std::collections::{HashMap, HashSet};

const LINE_H: f32 = 20.0;

/// What the sidebar knows of one schema's routines.
pub(in crate::app) enum Loaded {
    Wait,
    Ok(Vec<Routine>),
    Err(String),
}

/// What a click in the sidebar group asks for, done after the frame is drawn.
pub(in crate::app) enum RAct {
    /// Read the list of (database, schema).
    Load(String, String),
    /// Read it again after a failure.
    Reload(String, String),
    Fold(String),
    Open(String, String, Routine),
    Run(String, String, Routine),
    New(String, String, Kind),
    Drop(String, String, Routine),
}

#[derive(Default)]
pub(in crate::app) struct Routines {
    lists: HashMap<(u64, String), Loaded>,
    folded: HashSet<(u64, String)>,
    win: Option<RoutineWin>,
}

impl Routines {
    pub(in crate::app) fn window_open(&self) -> bool {
        self.win.is_some()
    }
}

#[derive(Default, PartialEq, Clone, Copy)]
enum Pane {
    #[default]
    View,
    Run,
    Drop,
}

/// The preview of what a save sends, kept while the text and the drop-and-create choice stay as they are.
type Memo = (String, bool, Result<Plan, String>);

pub(in crate::app) struct RoutineWin {
    conn: u64,
    /// The sidebar's database, and its namespace: the schema on PostgreSQL, the database again on MySQL.
    db: String,
    schema: String,
    dialect: db::sql::Dialect,
    /// A schema that belongs to the server: shown, never changed.
    system: bool,
    kind: Kind,
    /// None while a new routine is being written.
    routine: Option<Routine>,
    detail: Option<Detail>,
    /// The read in flight.
    req: u64,
    /// A save or a drop in flight.
    busy: bool,
    error: Option<String>,
    text: String,
    base: String,
    editing: bool,
    recreate: bool,
    /// PostgreSQL refused to replace the routine in place, so the window offers a drop and create.
    offer_recreate: bool,
    /// The text before the last save and whether that save dropped and created, for Undo.
    undo: Option<(String, bool)>,
    pane: Pane,
    typed: String,
    args: Vec<Arg>,
    memo: Option<Memo>,
}

impl RoutineWin {
    fn is(&self, conn: u64, db: &str, schema: &str, r: &Routine) -> bool {
        self.conn == conn && self.db == db && self.schema == schema && self.routine.as_ref().is_some_and(|x| x.key() == r.key())
    }

    fn dirty(&self) -> bool {
        self.text.trim() != self.base.trim()
    }

    fn title(&self) -> String {
        match &self.routine {
            Some(r) => r.label(self.dialect),
            None => format!("New {}", self.kind.noun()),
        }
    }
}

/// The ROUTINES caption of the sidebar: a painted chevron, the label and the count, like the TABLES and VIEWS captions.
fn caption_row(ui: &mut egui::Ui, hidden: bool, n: usize) -> egui::Response {
    let (cap, hr) = ui.allocate_exact_size(egui::vec2(ui.available_width(), SIDE_ROW_H - 4.0), Sense::click());
    let g = ui.painter().layout_job(LayoutJob::single_section("ROUTINES".into(), ui_fmt(10.0, FAINT(), W_SEMIBOLD, 0.6)));
    if state_of(&hr) > 0 {
        ui.painter().rect_filled(cap, R_SM, hover_fill(state_of(&hr)));
    }
    let c = egui::pos2(cap.left() + 12.0, cap.center().y + 1.0);
    let tri = if hidden { vec![c + egui::vec2(-2.0, -3.5), c + egui::vec2(-2.0, 3.5), c + egui::vec2(2.5, 0.0)] } else { vec![c + egui::vec2(-3.5, -2.0), c + egui::vec2(3.5, -2.0), c + egui::vec2(0.0, 2.5)] };
    ui.painter().add(egui::Shape::convex_polygon(tri, FAINT(), Stroke::NONE));
    ui.painter().galley(egui::pos2(cap.left() + 22.0, cap.center().y - g.size().y / 2.0 + 2.0), g, FAINT());
    ui.painter().text(egui::pos2(cap.right() - 8.0, cap.center().y + 2.0), egui::Align2::RIGHT_CENTER, n.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
    hr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Routines"));
    hr.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl App {
    fn routines_key(&self, conn: u64, db: &str, schema: &str) -> (u64, String) {
        (conn, ns_key(db, if self.is_pg(conn) { schema } else { "" }))
    }

    /// How many routines of a namespace match the sidebar's filter, so a database whose tables all miss it still shows.
    pub(in crate::app) fn routines_matching(&self, conn: u64, ns: &str, filter: &str) -> usize {
        match self.routines.lists.get(&(conn, ns.to_string())) {
            Some(Loaded::Ok(l)) if !filter.is_empty() => l.iter().filter(|r| r.name.to_lowercase().contains(filter)).count(),
            _ => 0,
        }
    }

    /// Forgets the lists of a database, so the next frame reads them again. Refresh on a database calls it.
    pub(in crate::app) fn routines_forget(&mut self, conn: u64, db: &str) {
        let pre = format!("{db}\u{1f}");
        self.routines.lists.retain(|k, _| k.0 != conn || (k.1 != db && !k.1.starts_with(&pre)));
    }

    /// The items of the database menu and the group's menu that start a new routine. PostgreSQL's stay enabled on a locked
    /// connection and ask to unlock when saving, like the structure editor; MySQL's wait for the unlock.
    pub(in crate::app) fn routines_new_items(ui: &mut egui::Ui, db_name: &str, schema: &str, pg: bool, writable: bool, system: bool, acts: &mut Vec<RAct>) {
        let on = !system && (pg || writable);
        let why = if system { "Belongs to the server" } else { "Read-only: unlock writes to create a routine" };
        for k in [Kind::Function, Kind::Procedure] {
            if ui.add_enabled(on, egui::Button::new(format!("New {}…", k.noun()))).on_disabled_hover_text(why).clicked() {
                acts.push(RAct::New(db_name.into(), schema.into(), k));
                ui.close();
            }
        }
    }

    /// The ROUTINES group under a database (MySQL) or the picked schema (PostgreSQL). Nothing when the schema has none.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn routines_group(&self, ui: &mut egui::Ui, conn: u64, db_name: &str, schema: &str, filter: &str, pg: bool, writable: bool, system: bool, acts: &mut Vec<RAct>) {
        let key = self.routines_key(conn, db_name, schema);
        let list = match self.routines.lists.get(&key) {
            None => return acts.push(RAct::Load(db_name.into(), schema.into())),
            Some(Loaded::Wait) => return,
            Some(Loaded::Err(e)) => {
                let r = side_row(ui, SIDE_ROW_H, 18.0, false, "Could not list routines. Retry", |p, r| {
                    p.text(egui::pos2(r.left() + 6.0, r.center().y), egui::Align2::LEFT_CENTER, "Could not list routines. Retry", FontId::proportional(TXT_SMALL), ALERT());
                });
                if r.on_hover_text(e.as_str()).clicked() {
                    acts.push(RAct::Reload(db_name.into(), schema.into()));
                }
                return;
            }
            Some(Loaded::Ok(l)) => l,
        };
        let shown: Vec<&Routine> = list.iter().filter(|r| filter.is_empty() || r.name.to_lowercase().contains(filter)).collect();
        if shown.is_empty() {
            return;
        }
        let hidden = filter.is_empty() && self.routines.folded.contains(&key);
        let cap = caption_row(ui, hidden, shown.len());
        if cap.clicked() {
            acts.push(RAct::Fold(key.1.clone()));
        }
        cap.context_menu(|ui| {
            menu_look(ui);
            Self::routines_new_items(ui, db_name, schema, pg, writable, system, acts);
        });
        if hidden {
            return;
        }
        let dialect = self.dialect_of(conn);
        for r in shown {
            let active = self.routines.win.as_ref().is_some_and(|w| w.is(conn, db_name, schema, r));
            let label = r.label(dialect);
            let func = r.kind == Kind::Function;
            let row = side_row(ui, SIDE_ROW_H, 18.0, active, &label, |p, rect| {
                p.text(egui::pos2(rect.left(), rect.center().y), egui::Align2::LEFT_CENTER, if func { "fn" } else { "pr" }, FontId::monospace(9.0), if active { PRIMARY() } else { C_KEYWORD() });
                // One line cut with an ellipsis: the row has a fixed height, so a wrapped name would draw over its neighbours.
                let mut job = egui::text::LayoutJob::simple_singleline(label.clone(), FontId::proportional(TXT_TITLE), if active { BRIGHT() } else { TEXT() });
                job.wrap = egui::text::TextWrapping::truncate_at_width((rect.width() - 28.0).max(0.0));
                let galley = p.layout_job(job);
                p.galley(egui::pos2(rect.left() + 20.0, rect.center().y - galley.size().y / 2.0), galley, TEXT());
            });
            let tip = if r.returns.is_empty() { format!("{}: {label}", r.kind.noun()) } else { format!("{}: {label}\nreturns {}", r.kind.noun(), r.returns) };
            let row = row.on_hover_text(tip);
            if row.clicked() {
                acts.push(RAct::Open(db_name.into(), schema.into(), r.clone()));
            }
            row.context_menu(|ui| {
                menu_look(ui);
                ui.label(RichText::new(format!("{}: {}", r.kind.noun(), r.name)).size(TXT_SMALL).color(MUTED()));
                if ui.button("Open").clicked() {
                    acts.push(RAct::Open(db_name.into(), schema.into(), r.clone()));
                    ui.close();
                }
                if ui.button(if func { "Run…" } else { "Call…" }).clicked() {
                    acts.push(RAct::Run(db_name.into(), schema.into(), r.clone()));
                    ui.close();
                }
                ui.separator();
                let on = !system && (pg || writable);
                if ui.add_enabled_ui(on, |ui| danger_item(ui, format!("Drop {}…", r.kind.noun()))).inner.on_disabled_hover_text(if system { "Belongs to the server" } else { "Read-only: unlock writes to drop a routine" }).clicked() {
                    acts.push(RAct::Drop(db_name.into(), schema.into(), r.clone()));
                    ui.close();
                }
            });
        }
    }

    pub(in crate::app) fn routine_act(&mut self, conn: u64, a: RAct) {
        match a {
            RAct::Load(db, schema) => self.routines_load(conn, db, schema),
            RAct::Reload(db, schema) => {
                let key = self.routines_key(conn, &db, &schema);
                self.routines.lists.remove(&key);
                self.routines_load(conn, db, schema);
            }
            RAct::Fold(ns) => {
                let key = (conn, ns);
                if !self.routines.folded.remove(&key) {
                    self.routines.folded.insert(key);
                }
            }
            RAct::Open(db, schema, r) => self.routine_open(conn, db, schema, Some(r), Pane::View),
            RAct::Run(db, schema, r) => self.routine_open(conn, db, schema, Some(r), Pane::Run),
            RAct::New(db, schema, kind) => self.routine_new(conn, db, schema, kind),
            RAct::Drop(db, schema, r) => self.routine_open(conn, db, schema, Some(r), Pane::Drop),
        }
    }

    fn routines_load(&mut self, conn: u64, db: String, schema: String) {
        let key = self.routines_key(conn, &db, &schema);
        // A connection without a session (the offline demo) has no routines to read; an empty list ends the asking.
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()).filter(|_| !self.demo) else {
            self.routines.lists.entry(key).or_insert(Loaded::Ok(Vec::new()));
            return;
        };
        self.routines.lists.insert(key, Loaded::Wait);
        self.worker.sql_routines(conn, sql.at(&db), db, schema);
    }

    fn routine_window_for(&self, conn: u64, db: String, schema: String, kind: Kind, routine: Option<Routine>) -> RoutineWin {
        let dialect = self.dialect_of(conn);
        let system = if dialect == db::sql::Dialect::Postgres { db::sql::pg::create::system_schema(&schema) } else { db::sql::create::SYSTEM_SCHEMAS.contains(&db.as_str()) };
        RoutineWin { conn, db, schema, dialect, system, kind, routine, detail: None, req: 0, busy: false, error: None, text: String::new(), base: String::new(), editing: false, recreate: false, offer_recreate: false, undo: None, pane: Pane::View, typed: String::new(), args: Vec::new(), memo: None }
    }

    fn routine_open(&mut self, conn: u64, db: String, schema: String, r: Option<Routine>, pane: Pane) {
        let Some(r) = r else { return };
        let mut w = self.routine_window_for(conn, db.clone(), schema.clone(), r.kind, Some(r.clone()));
        w.pane = pane;
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()).filter(|_| !self.demo) {
            self.req += 1;
            w.req = self.req;
            self.worker.sql_routine_detail(conn, self.req, sql.at(&db), schema, r);
        }
        self.routines.win = Some(w);
    }

    fn routine_new(&mut self, conn: u64, db: String, schema: String, kind: Kind) {
        let mut w = self.routine_window_for(conn, db, schema.clone(), kind, None);
        w.text = rt::template(w.dialect, kind, &schema);
        w.editing = true;
        self.routines.win = Some(w);
    }

    /// An answer of the worker. A read or a change answers the window that asked, by its request.
    pub(in crate::app) fn routine_done(&mut self, conn: u64, req: u64, ans: rt::Answer) {
        match ans {
            rt::Answer::List { database, schema, res } => {
                let key = self.routines_key(conn, &database, &schema);
                // A list asked for before a Refresh forgot it is not put back.
                if !matches!(self.routines.lists.get(&key), Some(Loaded::Wait)) {
                    return;
                }
                self.routines.lists.insert(key, match res {
                    Ok(l) => Loaded::Ok(l),
                    Err(e) => Loaded::Err(e),
                });
            }
            rt::Answer::Detail { res } => {
                let Some(w) = self.routines.win.as_mut().filter(|w| w.req == req) else { return };
                w.req = 0;
                match res {
                    Ok(d) => {
                        // Typing in the editor is kept when the read arrives after it started.
                        let typing = w.editing && w.dirty();
                        w.base = d.source.clone();
                        if !typing {
                            w.text = d.source.clone();
                        }
                        w.args = rt::inputs(&d).iter().map(|_| Arg::default()).collect();
                        w.detail = Some(*d);
                        w.error = None;
                    }
                    Err(e) => w.error = Some(e),
                }
            }
            rt::Answer::Changed { res } => {
                let Some(w) = self.routines.win.as_mut().filter(|w| w.req == req) else { return };
                w.req = 0;
                w.busy = false;
                match res {
                    Ok(r) => self.routine_changed(r),
                    Err(f) => {
                        w.offer_recreate = f.recreate;
                        w.error = Some(f.why);
                    }
                }
            }
        }
    }

    /// A save or a drop went through. `r` is the routine as it is now, None after a drop.
    fn routine_changed(&mut self, r: Option<Routine>) {
        let Some(mut w) = self.routines.win.take() else { return };
        let (conn, db, schema) = (w.conn, w.db.clone(), w.schema.clone());
        let list_key = self.routines_key(conn, &db, &schema);
        self.routines.lists.remove(&list_key);
        match r {
            None => self.ok(format!("Dropped the {}", w.kind.noun())),
            Some(r) => {
                let was = w.routine.is_some();
                if was && let Some(old) = &w.detail {
                    w.undo = Some((old.source.clone(), w.recreate));
                } else {
                    w.undo = None;
                }
                self.ok(if was { format!("Saved the {}", w.kind.noun()) } else { format!("Created the {}", w.kind.noun()) });
                w.routine = Some(r.clone());
                w.kind = r.kind;
                w.editing = false;
                w.recreate = false;
                w.offer_recreate = false;
                w.error = None;
                w.memo = None;
                // Read again, so the view shows what the server holds now and Undo has the current routine to work on.
                w.base = String::new();
                w.detail = None;
                if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()).filter(|_| !self.demo) {
                    self.req += 1;
                    w.req = self.req;
                    self.worker.sql_routine_detail(conn, self.req, sql.at(&db), schema, r);
                }
                self.routines.win = Some(w);
            }
        }
    }

    /// Starts a save, asking to unlock first when the connection is locked.
    fn routine_save(&mut self, restore: bool) {
        let Some(w) = self.routines.win.as_ref() else { return };
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        if !self.conn_writable(w.conn) {
            let conn = w.conn;
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |a: &mut App| a.routine_save(restore)))));
            return;
        }
        let Some(w) = self.routines.win.as_mut() else { return };
        // Undo writes the text from before the last save, on the routine as it is now, the way the save went.
        let (text, recreate) = match (restore, w.undo.clone()) {
            (true, Some((old, recreate))) => (old, recreate),
            _ => (w.text.clone(), w.recreate),
        };
        let (conn, db, schema, old) = (w.conn, w.db.clone(), w.schema.clone(), w.detail.clone());
        // An edit waits for the read of the routine it changes.
        if w.routine.is_some() && old.is_none() {
            return;
        }
        w.recreate = recreate;
        w.busy = true;
        w.error = None;
        self.req += 1;
        w.req = self.req;
        let (req, read_only) = (self.req, !self.conn_writable(conn));
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return self.warn_offline() };
        self.worker.sql_routine_save(conn, req, sql.at(&db), schema, old, text, recreate, read_only);
    }

    fn routine_drop(&mut self) {
        let Some(w) = self.routines.win.as_ref() else { return };
        let (conn, db, schema, Some(r)) = (w.conn, w.db.clone(), w.schema.clone(), w.routine.clone()) else { return };
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        if !self.conn_writable(conn) {
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |a: &mut App| a.routine_drop()))));
            return;
        }
        self.req += 1;
        let (req, read_only) = (self.req, !self.conn_writable(conn));
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return self.warn_offline() };
        if let Some(w) = self.routines.win.as_mut() {
            (w.req, w.busy, w.error) = (req, true, None);
        }
        self.worker.sql_routine_drop(conn, req, sql.at(&db), schema, r, read_only);
    }

    /// Runs the call in a new Query tab, where its results show. A routine that may write needs the connection unlocked; a PostgreSQL
    /// function declared STABLE or IMMUTABLE runs on a locked one, on the session's read-only guard, which refuses it if it writes anyway.
    fn routine_run(&mut self) {
        let Some(w) = self.routines.win.as_ref() else { return };
        let Some(detail) = w.detail.as_ref() else { return };
        let (conn, db, schema) = (w.conn, w.db.clone(), w.schema.clone());
        let bs = self.conn(conn).and_then(|c| c.sql.as_ref()).is_none_or(|s| s.backslash_escapes());
        let Some(script) = rt::run_script(w.dialect, &schema, detail, &w.args, bs) else { return };
        if !detail.read_only && !self.conn_writable(conn) {
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |a: &mut App| a.routine_run_script(conn, db, script)))));
            return;
        }
        self.routine_run_script(conn, db, script);
    }

    fn routine_run_script(&mut self, conn: u64, db: String, script: String) {
        self.sql_new_query(conn, db);
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.query = script;
        }
        self.sql_run(true, false);
        self.routines.win = None;
    }

    pub(in crate::app) fn routine_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.routines.win.take() else { return };
        let writable = self.conn_writable(w.conn);
        let session_mode = self.conn(w.conn).and_then(|c| c.sql.as_ref()).map(|s| s.sql_mode.clone());
        let conn_name = self.conn(w.conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        let screen = ctx.content_rect();
        let mut acts = WinActs::default();
        let r = banded(egui::Id::unique(("quarry-layer", "routine-window"))).show(ctx, |ui| {
            ui.set_width(fit_width(ctx, (screen.width() * 0.6).clamp(640.0, 920.0)));
            let ns = if w.dialect == db::sql::Dialect::Postgres { format!("{} / {}", w.db, w.schema) } else { w.db.clone() };
            let sub = format!("{conn_name} · {} · {ns}", w.kind.noun());
            dialog_head(ui, &Head::new(w.title()).sub(sub).glyph(glyphs::code));
            dialog_pane(ui, |ui| match w.pane {
                Pane::View => view_pane(ui, &mut w, writable, session_mode.as_deref(), screen.height()),
                Pane::Run => run_pane(ui, &mut w, writable),
                Pane::Drop => drop_pane(ui, &mut w),
            });
            foot(ui, &mut w, writable, &mut acts)
        });
        if let Some(i) = acts.pane {
            w.pane = i;
            w.typed.clear();
        }
        let keep = r.inner && !(r.should_close() && self.modal.is_none());
        let (save, undo, drop, run) = (acts.save, acts.undo, acts.drop, acts.run);
        if keep {
            self.routines.win = Some(w);
        }
        if save {
            self.routine_save(false);
        }
        if undo {
            self.routine_save(true);
        }
        if drop {
            self.routine_drop();
        }
        if run {
            self.routine_run();
        }
    }

    /// Opens the window on a made-up routine and its list, for the screenshot switches (QUARRY_TEST_SQL=routines, QUARRY_TEST_PG=routines).
    pub(in crate::app) fn demo_routines(&mut self, conn: u64, db: &str, schema: &str, pg: bool, view: &str) {
        let dialect = self.dialect_of(conn);
        let list = demo_list(pg);
        let key = self.routines_key(conn, db, schema);
        self.routines.lists.insert(key, Loaded::Ok(list.clone()));
        self.open_dbs.insert((conn, db.to_string()));
        let pick = if view == "new" { None } else { list.iter().find(|r| r.kind == if view == "run" { Kind::Procedure } else { Kind::Function }).cloned() };
        let mut w = self.routine_window_for(conn, db.into(), schema.into(), Kind::Function, pick.clone());
        match pick {
            Some(r) => {
                let d = demo_detail(dialect, &r);
                w.base = d.source.clone();
                w.text = d.source.clone();
                w.args = rt::inputs(&d).iter().map(|_| Arg::default()).collect();
                w.detail = Some(d);
                w.kind = r.kind;
                w.pane = if view == "run" { Pane::Run } else if view == "drop" { Pane::Drop } else { Pane::View };
                if view == "edit" {
                    w.editing = true;
                    w.text = w.text.replace("a + b", "a + b + 1");
                }
            }
            None => {
                w.text = rt::template(dialect, Kind::Function, schema);
                w.editing = true;
            }
        }
        self.routines.win = Some(w);
    }
}

#[derive(Default)]
struct WinActs {
    save: bool,
    undo: bool,
    drop: bool,
    run: bool,
    pane: Option<Pane>,
}

/// One row of the facts card: a caption and the value in monospace.
fn fact(ui: &mut egui::Ui, name: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    ui.label(RichText::new(name).size(TXT_SMALL).color(MUTED()));
    ui.add(egui::Label::new(RichText::new(value).font(FontId::monospace(TXT_SMALL)).color(TEXT())).wrap());
    ui.end_row();
}

fn facts(ui: &mut egui::Ui, d: &Detail, dialect: db::sql::Dialect) {
    plain_card(ui, |ui| {
        egui::Grid::new("routine-facts").num_columns(2).spacing([18.0, 6.0]).show(ui, |ui| {
            fact(ui, "Language", &d.lang);
            fact(ui, "Arguments", &d.signature);
            fact(ui, "Returns", &d.returns);
            let pg = dialect == db::sql::Dialect::Postgres;
            fact(ui, "Security", &if d.owner.is_empty() { d.security.clone() } else if pg { format!("{} (owner {})", d.security, d.owner) } else { format!("{} ({})", d.security, d.owner) });
            fact(ui, if pg { "Volatility" } else { "Determinism" }, &d.behavior);
            fact(ui, "Data access", &d.data_access);
            fact(ui, "Comment", &d.comment);
        });
    });
}

/// The SQL text area with line numbers and token colors, read-only unless `editable`. Returns whether the text changed.
fn code_editor(ui: &mut egui::Ui, text: &mut String, dialect: db::sql::Dialect, editable: bool, height: f32) -> bool {
    let id = egui::Id::unique("routine-source");
    let focused = ui.memory(|m| m.has_focus(id));
    let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        crate::ui::editor::memo_galley(ui, 1, buf.as_str(), wrap, || {
            let mut job = crate::ui::editor::sql_job_in(buf.as_str(), TXT_SMALL, dialect);
            for sec in &mut job.sections {
                sec.format.line_height = Some(LINE_H);
            }
            job
        })
    };
    let mut changed = false;
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused && editable { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt("routine-source-scroll").max_height(height).auto_shrink([false, true]).show(ui, |ui| {
            let out = crate::ui::vim::wrap(ui, id, text, |ui, text| {
                egui::TextEdit::multiline(text)
                    .id(id)
                    .font(FontId::monospace(TXT_SMALL))
                    .frame(crate::ui::editor::gutter_frame())
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, 96.0))
                    .interactive(editable)
                    .lock_focus(true)
                    .layouter(&mut layout)
                    .show(ui)
            });
            changed = out.response.changed();
            out.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, editable, "Routine definition"));
            crate::ui::editor::paint_gutter(ui, &out, None);
        });
    });
    changed
}

fn view_pane(ui: &mut egui::Ui, w: &mut RoutineWin, writable: bool, session_mode: Option<&str>, screen_h: f32) {
    if w.routine.is_some() && w.detail.is_none() && w.error.is_none() {
        ui.horizontal(|ui| {
            spin(ui);
            ui.label(RichText::new("Reading the routine…").color(MUTED()));
        });
        return;
    }
    if let Some(d) = &w.detail {
        facts(ui, d, w.dialect);
        ui.add_space(8.0);
        if let Some(why) = &d.header_missing {
            banner(ui, Notice::Warn, "Only the body is shown.", why, None);
            ui.add_space(8.0);
        }
    }
    if w.system {
        banner(ui, Notice::Info, "Belongs to the server.", "Its routines are shown but not changed here.", None);
        ui.add_space(8.0);
    }
    let blind = w.detail.as_ref().is_some_and(|d| d.header_missing.is_some());
    let can_edit = !w.system && !blind;
    let height = (screen_h - 560.0).clamp(160.0, 520.0);
    let editable = w.editing && can_edit;
    let changed = code_editor(ui, &mut w.text, w.dialect, editable, height);
    if changed {
        w.error = None;
    }
    if let Some(e) = w.error.clone() {
        ui.add_space(8.0);
        let lead = if w.offer_recreate { "PostgreSQL cannot replace it in place." } else { "Not done." };
        if banner(ui, if w.offer_recreate { Notice::Warn } else { Notice::Danger }, lead, &e, w.offer_recreate.then_some("Drop and create instead")) {
            w.recreate = true;
            w.offer_recreate = false;
            w.error = None;
        }
    }
    if !w.editing {
        return;
    }
    if let (Some(d), Some(now)) = (w.detail.as_ref().filter(|_| w.dirty()), session_mode) && !d.sql_mode.is_empty() && !db::sql::ddl::same_modes(&d.sql_mode, now) {
        ui.add_space(8.0);
        ui.label(RichText::new(format!("It was created with sql_mode {:?}. Saving creates it with this session's {:?}, which can change how its body runs.", d.sql_mode, now)).size(TXT_SMALL).color(WARNING()));
    }
    if !w.dirty() && w.routine.is_some() {
        return;
    }
    if !w.memo.as_ref().is_some_and(|m| m.0 == w.text && m.1 == w.recreate) {
        w.memo = Some((w.text.clone(), w.recreate, rt::plan(w.dialect, &w.schema, w.detail.as_ref(), &w.text, w.recreate)));
    }
    let Some((_, _, plan)) = &w.memo else { return };
    ui.add_space(8.0);
    caption(ui, "SQL TO RUN");
    ui.add_space(4.0);
    match plan {
        Ok(p) => {
            crate::app::sql::alter::sql_block(ui, "routine-sql", &p.script(), 160.0);
            for n in &p.notes {
                ui.add_space(4.0);
                ui.label(RichText::new(n.as_str()).size(TXT_SMALL).color(WARNING()));
            }
            if !writable {
                ui.add_space(4.0);
                ui.label(RichText::new("Writes are locked: Save asks to unlock them.").size(TXT_SMALL).color(FAINT()));
            }
        }
        Err(e) => {
            ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
        }
    }
}

fn run_pane(ui: &mut egui::Ui, w: &mut RoutineWin, writable: bool) {
    let Some(d) = w.detail.clone() else {
        ui.horizontal(|ui| {
            spin(ui);
            ui.label(RichText::new("Reading the routine…").color(MUTED()));
        });
        return;
    };
    let inputs = rt::inputs(&d);
    if inputs.is_empty() {
        ui.label(RichText::new("It takes no arguments.").color(MUTED()));
    }
    for (p, a) in inputs.iter().zip(w.args.iter_mut()) {
        ui.horizontal(|ui| {
            ui.set_min_height(BUTTON_H);
            ui.add_sized([180.0, BUTTON_H], egui::Label::new(RichText::new(format!("{} {}", if p.name.is_empty() { "(unnamed)" } else { &p.name }, p.mode.label())).size(TXT_SMALL).color(MUTED())).truncate());
            ui.add_enabled(!a.null, egui::TextEdit::singleline(&mut a.value).font(egui::TextStyle::Monospace).desired_width(260.0).hint_text(p.ty.as_str())).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Argument {}", p.name)));
            ui.checkbox(&mut a.null, "NULL");
        });
    }
    for p in d.params.iter().filter(|p| p.mode == ArgMode::Out) {
        ui.label(RichText::new(format!("{} {} comes back in the result.", p.name, p.ty)).size(TXT_SMALL).color(FAINT()));
    }
    ui.add_space(8.0);
    let bs = true;
    if let Some(s) = rt::run_script(w.dialect, &w.schema, &d, &w.args, bs) {
        caption(ui, "SQL TO RUN");
        ui.add_space(4.0);
        crate::app::sql::alter::sql_block(ui, "routine-run-sql", &s, 160.0);
    }
    ui.add_space(6.0);
    if !d.read_only && !writable {
        banner(ui, Notice::Locked, "Writes are locked.", "A routine counts as a write unless PostgreSQL declares the function STABLE or IMMUTABLE. Run asks to unlock.", None);
    } else {
        ui.label(RichText::new("The result opens in a new Query tab.").size(TXT_SMALL).color(FAINT()));
    }
}

fn drop_pane(ui: &mut egui::Ui, w: &mut RoutineWin) {
    let name = w.routine.as_ref().map(|r| r.label(w.dialect)).unwrap_or_default();
    banner(ui, Notice::Danger, "This cannot be undone.", &format!("{name} is dropped for good. Grants made on it go with it, and whatever calls it fails afterwards."), None);
    ui.add_space(10.0);
    let expect = w.routine.as_ref().map(|r| r.name.clone()).unwrap_or_default();
    crate::app::modals::typed_confirm(ui, &expect, &mut w.typed);
}

/// The footer. Returns whether the window stays open.
fn foot(ui: &mut egui::Ui, w: &mut RoutineWin, writable: bool, acts: &mut WinActs) -> bool {
    let mut open = true;
    let busy = w.busy || (w.routine.is_some() && w.detail.is_none() && w.error.is_none());
    let blind = w.detail.as_ref().is_some_and(|d| d.header_missing.is_some());
    let can_edit = !w.system && !blind;
    dialog_foot(ui, |ui| {
        match w.pane {
            Pane::View if w.editing => {
                let ok = !busy && w.dirty() && w.memo.as_ref().is_some_and(|m| m.2.is_ok());
                let label = if w.busy { "Saving..." } else if w.routine.is_some() { "Save routine" } else { "Create routine" };
                if ui.add_enabled(ok, primary_button(ui, label, PRIMARY())).on_disabled_hover_text(if busy { "Working" } else { "Nothing to save yet" }).clicked() {
                    acts.save = true;
                }
                if quiet_enabled(ui, !w.busy, if w.routine.is_some() { "Revert" } else { "Cancel" }).clicked() {
                    if w.routine.is_none() {
                        open = false;
                    }
                    w.text = w.base.clone();
                    w.editing = false;
                    w.recreate = false;
                    w.offer_recreate = false;
                    w.error = None;
                }
            }
            Pane::View => {
                if primary(ui, "Close", PRIMARY()).on_hover_text("Esc").clicked() {
                    open = false;
                }
                if w.routine.is_some() {
                    if quiet_enabled(ui, can_edit && !busy, "Edit").on_disabled_hover_text(if w.system { "Belongs to the server" } else { "The definition could not be read in full" }).clicked() {
                        w.editing = true;
                    }
                    if quiet_enabled(ui, w.detail.is_some() && !w.busy, "Run…").clicked() {
                        acts.pane = Some(Pane::Run);
                    }
                }
            }
            Pane::Run => {
                if ui.add_enabled(w.detail.is_some(), primary_button(ui, "Run", PRIMARY())).clicked() {
                    acts.run = true;
                }
                if quiet(ui, "Back").clicked() {
                    acts.pane = Some(Pane::View);
                }
            }
            Pane::Drop => {
                let ok = !w.busy && w.routine.as_ref().is_some_and(|r| w.typed == r.name);
                if ui.add_enabled_ui(ok, |ui| danger(ui, if w.busy { "Dropping..." } else { "Drop" })).inner.clicked() {
                    acts.drop = true;
                }
                if quiet_enabled(ui, !w.busy, "Cancel").clicked() {
                    acts.pane = Some(Pane::View);
                }
            }
        }
        foot_left(ui, |ui| {
            if w.pane != Pane::View {
                return;
            }
            if w.busy {
                spin(ui);
            }
            if !w.editing && w.routine.is_some() {
                if ui.add_enabled_ui(!w.system && !busy, |ui| danger(ui, "Drop…")).inner.on_disabled_hover_text(if w.system { "Belongs to the server" } else { "Working" }).clicked() {
                    acts.pane = Some(Pane::Drop);
                }
                if w.undo.is_some() && quiet_enabled(ui, !busy, "Undo").on_hover_text("Put the definition from before the last save back").clicked() {
                    acts.undo = true;
                }
            }
            let note = if !writable && w.editing { "Writes are locked: Save asks to unlock them" } else { "" };
            if !note.is_empty() {
                ui.label(RichText::new(note).size(TXT_SMALL).color(MUTED()));
            }
        });
    });
    open
}

/// Made-up routines for the screenshot switches.
fn demo_list(pg: bool) -> Vec<Routine> {
    let f = |name: &str, args: &str, returns: &str, lang: &str| Routine { kind: Kind::Function, name: name.into(), args: args.into(), returns: returns.into(), lang: lang.into() };
    let p = |name: &str, args: &str, lang: &str| Routine { kind: Kind::Procedure, name: name.into(), args: args.into(), returns: String::new(), lang: lang.into() };
    if pg {
        vec![f("add_tax", "amount numeric, rate numeric", "numeric", "sql"), f("order_total", "order_id integer", "numeric", "plpgsql"), f("order_total", "order_id integer, with_tax boolean", "numeric", "plpgsql"), p("archive_orders", "before date", "plpgsql")]
    } else {
        vec![f("add_tax", "", "decimal(10,2)", "SQL"), f("order_total", "", "decimal(10,2)", "SQL"), p("archive_orders", "", "SQL")]
    }
}

fn demo_detail(d: db::sql::Dialect, r: &Routine) -> Detail {
    let pg = d == db::sql::Dialect::Postgres;
    let prm = |name: &str, mode: ArgMode, ty: &str| rt::Param { name: name.into(), mode, ty: ty.into() };
    let (source, params) = match (pg, r.kind) {
        (true, Kind::Function) => ("CREATE OR REPLACE FUNCTION public.add_tax(amount numeric, rate numeric)\n RETURNS numeric\n LANGUAGE sql\n IMMUTABLE\nAS $function$\n  SELECT amount + amount * rate\n$function$\n", vec![prm("amount", ArgMode::In, "numeric"), prm("rate", ArgMode::In, "numeric")]),
        (true, Kind::Procedure) => ("CREATE OR REPLACE PROCEDURE public.archive_orders(IN before date)\n LANGUAGE plpgsql\nAS $procedure$\nBEGIN\n  DELETE FROM orders WHERE placed_at < before;\nEND\n$procedure$\n", vec![prm("before", ArgMode::In, "date")]),
        (false, Kind::Function) => ("CREATE DEFINER=`shop`@`%` FUNCTION `add_tax`(amount DECIMAL(10,2), rate DECIMAL(4,2)) RETURNS decimal(10,2)\n    DETERMINISTIC\nRETURN amount + amount * rate", vec![prm("amount", ArgMode::In, "decimal(10,2)"), prm("rate", ArgMode::In, "decimal(4,2)")]),
        (false, Kind::Procedure) => ("CREATE DEFINER=`shop`@`%` PROCEDURE `archive_orders`(IN before_day DATE)\nBEGIN\n  DELETE FROM orders WHERE placed_at < before_day;\nEND", vec![prm("before_day", ArgMode::In, "date")]),
    };
    let signature = params.iter().map(|p| format!("{} {} {}", p.mode.label(), p.name, p.ty)).collect::<Vec<_>>().join(", ");
    Detail {
        routine: Some(r.clone()),
        source: source.into(),
        signature,
        returns: r.returns.clone(),
        lang: r.lang.clone(),
        security: if pg { "INVOKER".into() } else { "DEFINER".into() },
        owner: if pg { "analyst".into() } else { "shop@%".into() },
        behavior: if pg { "IMMUTABLE".into() } else { "DETERMINISTIC".into() },
        data_access: if pg { String::new() } else { "NO SQL".into() },
        comment: "Adds the sales tax to an amount.".into(),
        read_only: pg && r.kind == Kind::Function,
        params,
        ..Default::default()
    }
}
