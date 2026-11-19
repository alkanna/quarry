//! The Cmd+K form of a snippet: Enter on a script with `{{...}}` values asks for them in the palette's place, shows the
//! statement they make and runs it. A read runs at once; a write asks first, and on a locked connection goes through
//! Unlock and comes back to that question.
use super::*;
use crate::library::snippet::{self, Ty};
use crate::library::ScriptRef;

const FORM_W: f32 = 640.0;
const LABEL_W: f32 = 120.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum Stage {
    Fill,
    /// A write on a writable connection, waiting for Run.
    Confirm,
}

pub(in crate::app) struct SnippetForm {
    pub id: u64,
    pub name: String,
    pub body: String,
    pub conn: u64,
    pub db: String,
    pub schema: String,
    pub values: snippet::Values,
    pub stage: Stage,
    /// The palette's query, put back by Esc.
    pub query: String,
}

/// The bar at the bottom of the form: the keys, the confirm of a write, or Unlock on a locked connection.
#[derive(Clone, Copy, PartialEq)]
enum Bar {
    Keys,
    Confirm,
    Unlock,
}

/// What a click or key in the form asks for, done once the form is drawn.
enum Do {
    Back,
    Run,
    Edit,
    Unlock,
    Target(u64, String),
    MakeDefault,
}

impl App {
    /// Enter on a snippet in the palette: its values start from the last run's, else their defaults.
    pub(in crate::app) fn open_snippet_form(&mut self, id: u64, query: String) {
        let Some(s) = self.library.scripts.iter().find(|s| s.id == id) else { return self.warn("That script is no longer in the library.") };
        let p = snippet::parse(&s.body, self.dialect_of(s.conn));
        // By name, so a value renamed or moved in the statement never takes another one's last value.
        let mut values = self.lib_ui.snippet_values.get(&id).cloned().unwrap_or_default();
        for v in &p.vars {
            values.entry(v.name.clone()).or_insert_with(|| v.default.clone().unwrap_or_default());
        }
        let conn = if self.conn(s.conn).is_some() { s.conn } else { self.conns.iter().find(|c| c.meta.engine.is_sql()).map_or(s.conn, |c| c.meta.id) };
        let db = if s.db.is_empty() || conn != s.conn { self.default_db(conn) } else { s.db.clone() };
        // A PostgreSQL schema belongs to the script's own connection; another one starts from its own default.
        let schema = if conn == s.conn { s.schema.clone() } else { String::new() };
        let form = SnippetForm { id, name: s.name.clone(), body: s.body.clone(), conn, db, schema, values, stage: Stage::Fill, query };
        self.wake_conn(conn);
        self.modal = Some(Modal::Snippet(Box::new(form)));
    }

    /// A connection that is not open, or failed, starts connecting now, so it is ready by the time the values are in.
    fn wake_conn(&mut self, conn: u64) {
        if !self.demo && self.conn(conn).is_some_and(|c| matches!(c.status, Status::Idle | Status::Failed(_))) {
            self.connect(conn);
        }
    }

    /// The connections a snippet can run on: the ones of its own engine, since its SQL was written for that one.
    fn snippet_conns(&self, id: u64) -> Vec<u64> {
        let engine = self.library.scripts.iter().find(|s| s.id == id).and_then(|s| self.conn(s.conn)).map(|c| c.meta.engine);
        self.conns.iter().filter(|c| match engine {
            Some(e) => c.meta.engine == e,
            None => c.meta.engine.is_sql(),
        }).map(|c| c.meta.id).collect()
    }

    pub(in crate::app) fn snippet_form(&mut self, ctx: &egui::Context) {
        let Some(Modal::Snippet(mut f)) = self.modal.take() else { return };
        let top = ctx.content_rect().height() * 0.14;
        let id = egui::Id::unique(("quarry-layer", "snippet"));
        let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top));
        let d = self.dialect_of(f.conn);
        let p = snippet::parse(&f.body, d);
        let filled = snippet::fill(&f.body, &p, &f.values, d, self.sql_bs_of(f.conn));
        let writes = filled.as_ref().is_ok_and(|t| db::sql::lex::read_only_check(t, d).is_err());
        let bar = match (writes, self.conn_writable(f.conn), f.stage) {
            (true, false, _) => Bar::Unlock,
            (true, true, Stage::Confirm) => Bar::Confirm,
            _ => Bar::Keys,
        };
        let ready = self.conn(f.conn).is_some_and(|c| c.status == Status::Ready) || self.demo;
        let failed = self.conn(f.conn).is_some_and(|c| matches!(c.status, Status::Failed(_)));
        let saved_default = self.library.scripts.iter().find(|s| s.id == f.id).is_some_and(|s| s.conn == f.conn && (s.db == f.db || s.db.is_empty() && f.db == self.default_db(f.conn)));
        let cname = self.conn_name(f.conn);
        let prod = self.conn(f.conn).is_some_and(|c| c.meta.tag.eq_ignore_ascii_case("production"));
        let mut todo: Vec<Do> = Vec::new();
        let r = egui::Modal::new(id).area(area).frame(dialog_frame()).show(ctx, |ui| {
            let form_w = fit_width(ctx, FORM_W);
            ui.set_width(form_w);
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let (esc, enter, cmd_enter, cmd_e) = ui.input_mut(|i| {
                let cmd_enter = i.consume_key(egui::Modifiers::COMMAND, Key::Enter);
                (i.consume_key(egui::Modifiers::NONE, Key::Escape), i.consume_key(egui::Modifiers::NONE, Key::Enter), cmd_enter, i.consume_key(egui::Modifiers::COMMAND, Key::E))
            });
            // Header: back, the name, Edit and where it runs.
            ui.horizontal(|ui| {
                ui.set_height(PALETTE_HEAD_H);
                ui.add_space(12.0);
                if quiet(ui, "Back").on_hover_text("Back to the search (Esc)").clicked() {
                    todo.push(Do::Back);
                }
                ui.add_space(6.0);
                ui.label(semibold(RichText::new(&f.name).size(TXT_SEARCH).color(BRIGHT())));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(14.0);
                    let label = if f.db.is_empty() { cname.clone() } else { format!("{cname} \u{b7} {}", f.db) };
                    // Built only while the menu is open, and scrolled, since a server can hold hundreds of databases.
                    ui.menu_button(RichText::new(label).color(BRIGHT()), |ui| {
                        menu_list(ui);
                        egui::ScrollArea::vertical().max_height(LIST_MAX_H).show(ui, |ui| {
                            for c in self.snippet_conns(f.id) {
                                let Some(conn) = self.conn(c) else { continue };
                                ui.horizontal(|ui| {
                                    ui.add_space(10.0);
                                    caption(ui, conn.meta.name.to_uppercase());
                                });
                                let here = (c == f.conn).then_some(f.db.as_str()).unwrap_or_default();
                                let dbs: Vec<&str> = if conn.dbs.is_empty() { vec![here] } else { conn.dbs.iter().map(|d| d.0.as_str()).collect() };
                                for db in dbs {
                                    let text = if db.is_empty() { conn.meta.name.as_str() } else { db };
                                    if ui.selectable_label(c == f.conn && db == f.db, text).clicked() {
                                        todo.push(Do::Target(c, db.to_string()));
                                        ui.close();
                                    }
                                }
                            }
                        });
                    });
                    if !saved_default && quiet(ui, "Make default").on_hover_text("Run this snippet here from now on").clicked() {
                        todo.push(Do::MakeDefault);
                    }
                    if quiet(ui, format!("Edit  {}", keys(false, false, "E"))).on_hover_text("Open the snippet in a tab to change it").clicked() {
                        todo.push(Do::Edit);
                    }
                });
            });
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() - 0.5, Stroke::new(1.0, RAISED_LINE()));
            ui.add_space(10.0);
            // One row per value.
            let locked = f.stage == Stage::Confirm;
            for (i, v) in p.vars.iter().enumerate() {
                let val = f.values.entry(v.name.clone()).or_default();
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.vertical(|ui| {
                        ui.set_width(LABEL_W);
                        ui.label(RichText::new(&v.name).font(FontId::monospace(TXT_BODY)).color(BRIGHT()));
                        ui.label(RichText::new(v.ty.label()).size(TXT_CAPTION).color(FAINT()));
                    });
                    ui.add_enabled_ui(!locked, |ui| match &v.ty {
                        Ty::Choice(c) => {
                            let mut k = c.iter().position(|c| c == val);
                            let opts: Vec<(Option<usize>, &str)> = c.iter().enumerate().map(|(j, c)| (Some(j), c.as_str())).collect();
                            segmented(ui, &mut k, &opts);
                            if let Some(j) = k {
                                *val = c[j].clone();
                            }
                        }
                        _ => {
                            let fid = egui::Id::unique(("snippet-value", f.id, i));
                            let hint = Some(v.ty.hint()).filter(|h| !h.is_empty()).or(v.default.as_deref()).unwrap_or("");
                            well_field(ui, fid, FieldIcon::Plus, None, |ui| {
                                let w = form_w - LABEL_W - 80.0 - if v.ty == Ty::Date { 150.0 } else { 0.0 };
                                let r = ui.add(field_edit(val, fid, hint, w, egui::TextStyle::Monospace));
                                if i == 0 && f.stage == Stage::Fill && !ui.memory(|m| m.focused().is_some()) {
                                    r.request_focus();
                                }
                                if v.ty == Ty::Date {
                                    use crate::ui::pickers::{local_now, sql_text, Civil};
                                    let day = |back: i64| sql_text(Civil { day: local_now().day - back, ms: 0 }, false);
                                    if quiet(ui, "yesterday").clicked() {
                                        *val = day(1);
                                    }
                                    if quiet(ui, "today").clicked() {
                                        *val = day(0);
                                    }
                                }
                            });
                        }
                    });
                });
                ui.add_space(8.0);
            }
            if let Some(e) = &p.err {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    field_note(ui, &e.msg, ALERT());
                });
            }
            // The statement it will run.
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                caption(ui, "WILL RUN");
                ui.add_space(8.0);
                if filled.is_ok() {
                    let (t, c) = if writes { ("WRITE \u{b7} ASKS FIRST", WARNING()) } else { ("READ \u{b7} RUNS AT ONCE", SUCCESS()) };
                    badge(ui, t, c);
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(12).show(ui, |ui| {
                    ui.set_width(form_w - 56.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| match &filled {
                        Ok(t) => {
                            // Laid out once per text, not every frame the form is open.
                            ui.label(crate::ui::editor::memo_galley(ui, 9, t, ui.available_width(), || crate::ui::editor::sql_job_in(t, TXT_BODY, d)));
                        }
                        Err(e) => {
                            ui.label(RichText::new(e).color(ALERT()));
                        }
                    });
                });
            });
            ui.add_space(14.0);
            // The footer: keys, or the question a write asks.
            let (band, _) = ui.allocate_exact_size(egui::vec2(form_w, if bar == Bar::Keys { PALETTE_FOOT_H } else { CONFIRM_BAR_H }), Sense::hover());
            let pnt = ui.painter().clone();
            let (fill, line) = match bar {
                Bar::Unlock => (tint(WARNING(), 22), WARNING()),
                Bar::Confirm => (tint(ALERT(), 22), ALERT()),
                Bar::Keys => (RAISED().lerp_to_gamma(PANEL(), 0.5), RAISED_LINE()),
            };
            crate::app::palette::paint_band(&pnt, band, R_LG, fill, line);
            if bar == Bar::Unlock {
                band_lines(&pnt, band, &format!("{cname} is read-only"), "Unlock it, then confirm here. The values stay filled.");
                let b = egui::Rect::from_min_size(band.right_center() - egui::vec2(126.0, BUTTON_H / 2.0), egui::vec2(110.0, BUTTON_H));
                if ui.put(b, primary_button(ui, "Unlock\u{2026}", WARNING())).clicked() || enter && ready {
                    todo.push(Do::Unlock);
                }
            } else if bar == Bar::Confirm {
                let tag = if prod { ", a Production connection" } else { "" };
                let sub = if ready { format!("{} runs as written. Undo can't reverse it.", verb(filled.as_deref().unwrap_or(""))) } else { format!("Waiting for {cname} to connect.") };
                band_lines(&pnt, band, &format!("Changes data on {cname}{tag}"), &sub);
                let b = egui::Rect::from_min_size(band.right_center() - egui::vec2(146.0, BUTTON_H / 2.0), egui::vec2(130.0, BUTTON_H));
                if (ui.put(b, primary_button(ui, &format!("Run {}", verb(filled.as_deref().unwrap_or(""))), ALERT())).clicked() || cmd_enter) && ready {
                    todo.push(Do::Run);
                }
            } else {
                let run = if ready { "run" } else if failed { "connection failed, retrying" } else { "connecting\u{2026}" };
                crate::app::palette::key_hints(&pnt, band.left() + 16.0, band.center().y, &[(&["tab"], "next value"), (&["\u{21b5}"], run)]);
                crate::app::palette::key_hint_right(&pnt, band.right() - 16.0, band.center().y, "esc", "back to search");
                if (enter || cmd_enter) && filled.is_ok() && ready {
                    todo.push(Do::Run);
                }
            }
            if esc {
                todo.push(Do::Back);
            }
            if cmd_e {
                todo.push(Do::Edit);
            }
        });
        let mut keep = !r.should_close() || todo.iter().any(|t| matches!(t, Do::Back));
        for t in todo {
            match t {
                Do::Back if f.stage == Stage::Confirm => f.stage = Stage::Fill,
                Do::Back => {
                    self.modal = Some(Modal::Palette { query: std::mem::take(&mut f.query), sel: 0, cmds: false, new_tab: false });
                    return;
                }
                Do::Target(c, db) => {
                    if c != f.conn {
                        f.schema = self.library.scripts.iter().find(|s| s.id == f.id && s.conn == c).map(|s| s.schema.clone()).unwrap_or_default();
                    }
                    f.conn = c;
                    f.db = db;
                    f.stage = Stage::Fill;
                    self.wake_conn(c);
                }
                Do::MakeDefault => self.set_snippet_default(f.id, f.conn, &f.db, &f.schema),
                Do::Edit => {
                    keep = false;
                    self.open_script(&ScriptRef::Saved(f.id), crate::app::library::open::How::Here);
                }
                Do::Unlock => {
                    let mut back = f;
                    back.stage = Stage::Confirm;
                    let conn = back.conn;
                    self.ask_unlock_on(conn, move |a| a.modal = Some(Modal::Snippet(back)));
                    return;
                }
                Do::Run if writes && f.stage == Stage::Fill => f.stage = Stage::Confirm,
                Do::Run => {
                    let Ok(text) = filled.clone() else { continue };
                    self.lib_ui.snippet_values.insert(f.id, f.values.clone());
                    self.run_snippet(&f, text);
                    return;
                }
            }
        }
        if keep && self.modal.is_none() {
            self.modal = Some(Modal::Snippet(f));
        }
    }

    /// Make default, from the form or a snippet's tab: the script runs on `conn` and `db` from Cmd+K from now on.
    pub(in crate::app) fn set_snippet_default(&mut self, id: u64, conn: u64, db: &str, schema: &str) {
        crate::library::ops::set_target(&mut self.library, id, conn, db, schema);
        self.save_library();
        let at = if db.is_empty() { self.conn_name(conn) } else { format!("{} \u{b7} {db}", self.conn_name(conn)) };
        self.ok(format!("Runs on {at} by default"));
    }

    /// Runs the filled statement in a Query tab on the form's target: the current tab when it holds nothing of the
    /// person's on that connection and database, else a new one. The tab is not bound to the script: its text is the
    /// filled statement, and Cmd+S on it saves a new script.
    fn run_snippet(&mut self, f: &SnippetForm, text: String) {
        let reuse = self.tab_clean(self.cur, "sql") && self.t().is_some_and(|t| t.sel.conn == f.conn && t.sel.db == f.db);
        if !reuse {
            self.sql_new_query(f.conn, f.db.clone());
        }
        if let Some(t) = self.tm() {
            t.script = None;
            if let Some(q) = t.sql.as_mut() {
                (q.snip_vals, q.snip_filled) = Default::default();
                q.query = text;
                q.caret = 0;
                q.run_schema = f.schema.clone();
                q.run_db = String::new();
            }
        }
        self.sql_run(true, false);
    }
}

/// The two lines of the confirm and unlock bars, each centered on its ink.
fn band_lines(p: &egui::Painter, band: egui::Rect, title: &str, sub: &str) {
    for (text, size, color, dy) in [(title, TXT_BODY, BRIGHT(), -9.0), (sub, TXT_SMALL, MUTED(), 10.0)] {
        let g = p.layout_no_wrap(text.to_string(), FontId::proportional(size), color);
        let at = egui::pos2(band.left() + 20.0, band.center().y + dy - g.size().y / 2.0 + optical_dy(&g));
        p.galley(at, g, color);
    }
}

/// The statement's first word, for "Run UPDATE".
fn verb(text: &str) -> String {
    let t = text.trim_start();
    t.split(|c: char| !c.is_ascii_alphabetic()).next().filter(|w| !w.is_empty()).map(|w| w.to_ascii_uppercase()).unwrap_or_else(|| "it".into())
}

impl App {
    /// QUARRY_TEST_SNIPPET=<state> over the MySQL demo: palette (Cmd+K finding two snippets), form (a read filled in),
    /// write (the confirm of a write), locked (a write on a locked connection), tab (a snippet open on Template), filled
    /// (the same on Filled) or make (Make it a value on a plain script).
    pub(in crate::app) fn demo_snippet(&mut self, state: &str) {
        use crate::library::Script;
        (self.conns, self.tabs) = (Vec::new(), Vec::new());
        self.open_dbs.clear();
        self.load_sql_demo();
        self.sql_query_demo();
        self.demo_scripts();
        let folder = |a: &App, name: &str| a.library.folders.iter().find(|f| f.name == name).map(|f| f.id);
        let add = |a: &mut App, name: &str, body: &str, folder: Option<u64>| {
            let id = a.library.next_id();
            a.library.scripts.push(Script { id, name: name.into(), kind: "sql".into(), body: body.into(), conn: 3, db: "shop".into(), folder, updated: 1_731_400_000, ..Default::default() });
            id
        };
        let (reports, maint) = (folder(self, "Reports"), folder(self, "Maintenance"));
        let read = add(self, "Orders on a day", "SELECT o.id, o.customer_id, o.total, o.status\nFROM orders o\nWHERE DATE(o.created_at) = {{day:date}}\n  AND o.status = {{status:paid|pending|refunded=paid}}\nORDER BY o.created_at\nLIMIT {{limit:int=100}};", reports);
        let write = add(self, "Mark orders shipped", "UPDATE orders SET status = 'shipped', shipped_at = NOW()\nWHERE id IN ({{ids:int list}});", maint);
        let named = |v: &[(&str, &str)]| -> snippet::Values { v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
        self.lib_ui.snippet_values.insert(read, named(&[("day", "2026-11-03"), ("status", "paid"), ("limit", "100")]));
        self.lib_ui.snippet_values.insert(write, named(&[("ids", "4405, 4407, 4412")]));
        match state {
            "palette" => self.open_palette_with("orders".into()),
            "form" => self.open_snippet_form(read, "orders".into()),
            "write" | "locked" => {
                if state == "write"
                    && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == 3)
                {
                    c.writable = true;
                }
                self.open_snippet_form(write, "orders".into());
                if let Some(Modal::Snippet(f)) = self.modal.as_mut()
                    && state == "write"
                {
                    f.stage = Stage::Confirm;
                }
            }
            "tab" | "filled" => {
                self.open_script(&ScriptRef::Saved(read), crate::app::library::open::How::Here);
                if let Some(s) = self.tm().and_then(|t| t.sql.as_mut()) {
                    s.snip_vals = [("day", "2026-11-03"), ("status", "paid"), ("limit", "100")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                    s.snip_filled = state == "filled";
                }
            }
            "make" => {
                let plain = add(self, "Orders of one day", "SELECT o.id, o.total FROM orders o WHERE DATE(o.created_at) = '2026-11-03';", reports);
                self.open_script(&ScriptRef::Saved(plain), crate::app::library::open::How::Here);
                if let Some(at) = self.t().and_then(|t| t.sql.as_ref()).and_then(|s| s.query.find("'2026-11-03'")) {
                    self.ask_make_value(at..at + 12);
                }
            }
            _ => {}
        }
    }
}
