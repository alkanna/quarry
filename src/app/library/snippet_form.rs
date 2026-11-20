//! The Cmd+K form of a snippet: Enter on a script with `{{...}}` values asks for them in the palette's place, shows the
//! statement they make and runs it. A read runs at once; a write asks first, and on a locked connection goes through
//! Unlock and comes back to that question.
use super::*;
use crate::library::snippet::{self, Ty};
use super::snippet_strip::Src;
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
    /// The script's kind (sql, shell or filter), its text, and for a filter its sort, projection and collection.
    pub kind: String,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub coll: String,
    /// None while the snippet's own connection was removed and the person has not picked another.
    pub conn: Option<u64>,
    pub db: String,
    pub schema: String,
    pub values: snippet::Values,
    pub stage: Stage,
    /// The palette's query, put back by Esc. None when the form was opened from elsewhere, where Esc just closes it.
    pub query: Option<String>,
    /// The filled statement, whether it writes and its verb, kept until a value or the target changes: the read-only
    /// check lexes the text several times, too much for every frame.
    pub memo: Option<(u64, Result<String, String>, bool, String)>,
    /// The target menu's filter, cleared when the menu closes.
    pub target_q: String,
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
    Retry,
}

impl App {
    /// Enter on a snippet in the palette: its values start from the last run's, else their defaults.
    pub(in crate::app) fn open_snippet_form(&mut self, id: u64, query: Option<String>) {
        let Some(s) = self.library.scripts.iter().find(|s| s.id == id) else { return self.warn("That script is no longer in the library.") };
        let p = self.script_snippet(s).unwrap_or_default();
        // By name, so a value renamed or moved in the statement never takes another one's last value.
        let mut values = self.lib_ui.snippet_values.get(&id).cloned().unwrap_or_default();
        for v in &p.vars {
            values.entry(v.name.clone()).or_insert_with(|| v.default.clone().unwrap_or_default());
        }
        // A removed connection is never swapped for another at random, since it could be production: the person picks one.
        let conn = self.conn(s.conn).is_some().then_some(s.conn);
        let form = SnippetForm { id, name: s.name.clone(), kind: s.kind.clone(), body: s.body.clone(), sort: s.sort.clone(), project: s.project.clone(), coll: s.coll.clone(), conn, db: s.db.clone(), schema: s.schema.clone(), values, stage: Stage::Fill, query, memo: None, target_q: String::new() };
        if let Some(c) = conn {
            self.wake_conn(c);
        }
        self.modal = Some(Modal::Snippet(Box::new(form)));
    }

    /// A connection that is not open, or failed, starts connecting now, so it is ready by the time the values are in.
    fn wake_conn(&mut self, conn: u64) {
        if !self.demo && self.conn(conn).is_some_and(|c| c.status.can_connect()) {
            self.connect(conn);
        }
    }

    /// The connections a snippet can run on: the ones of its own engine, since its SQL was written for that one.
    fn snippet_conns(&self, id: u64) -> Vec<u64> {
        let Some(s) = self.library.scripts.iter().find(|s| s.id == id) else { return Vec::new() };
        let engine = self.conn(s.conn).map(|c| c.meta.engine);
        self.conns.iter().filter(|c| match engine {
            Some(e) => c.meta.engine == e,
            // Its connection is gone: any one its kind runs on, never a Mongo one for SQL or the reverse.
            None => crate::library::format::kind_runs_on(&s.kind, c.meta.engine).unwrap_or(false),
        }).map(|c| c.meta.id).collect()
    }

    pub(in crate::app) fn snippet_form(&mut self, ctx: &egui::Context) {
        let Some(Modal::Snippet(mut f)) = self.modal.take() else { return };
        let top = ctx.content_rect().height() * 0.14;
        let id = egui::Id::unique(("quarry-layer", "snippet"));
        let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top));
        let c = f.conn.and_then(|id| self.conn(id));
        let d = f.conn.map_or_else(Default::default, |id| self.dialect_of(id));
        let l = form_lang(&f.kind, d);
        let p = snippet::merge(f.parts().into_iter().map(|t| snippet::parse(t, l)));
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            // The quoting depends on the dialect and backslash reading too, which change once the woken connection reads its sql_mode.
            (f.conn, d == db::sql::Dialect::Postgres, f.conn.is_none_or(|id| self.sql_bs_of(id)), &f.kind).hash(&mut h);
            for v in &p.vars {
                (&v.name, f.values.get(&v.name)).hash(&mut h);
            }
            h.finish()
        };
        if f.memo.as_ref().is_none_or(|m| m.0 != key) {
            let filled = self.form_filled(&f, &p, d);
            let (writes, what) = match (f.kind.as_str(), filled.as_deref()) {
                (_, Err(_)) => (false, String::new()),
                ("sql", Ok(t)) => (db::sql::lex::read_only_check(t, d).is_err(), verb(t, d)),
                // The Shell's own confirm, on what will run: a value is a literal, so it adds no write.
                ("shell", Ok(t)) => (!crate::app::shell::shell_ops(t, f.conn.and_then(|c| self.conn(c)).is_some_and(|c| c.meta.engine.is_sql()), std::iter::empty()).is_empty(), "script".into()),
                _ => (false, String::new()),
            };
            f.memo = Some((key, filled, writes, what));
        }
        let Some((_, filled, writes, what)) = f.memo.clone() else { return };
        let bar = match (writes, f.conn.is_some_and(|id| self.conn_writable(id)), f.stage) {
            (true, false, _) => Bar::Unlock,
            (true, true, Stage::Confirm) => Bar::Confirm,
            _ => Bar::Keys,
        };
        let ready = c.is_some_and(|c| c.status == Status::Ready || self.demo);
        let failed = c.is_some_and(|c| matches!(c.status, Status::Failed(_)));
        let saved_default = self.library.scripts.iter().find(|s| s.id == f.id).is_some_and(|s| f.conn.is_some_and(|c| self.runs_at(s, c, &f.db, &f.schema, "")));
        let cname = c.map_or_else(|| "Pick a connection".to_string(), |c| c.meta.name.clone());
        let prod = c.is_some_and(|c| c.meta.tag.eq_ignore_ascii_case("production"));
        let mut todo: Vec<Do> = Vec::new();
        let r = egui::Modal::new(id).area(area).frame(dialog_frame()).show(ctx, |ui| {
            let form_w = fit_width(ctx, FORM_W);
            ui.set_width(form_w);
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            // An open menu keeps Esc and Enter: Esc closes it and Enter picks its first match.
            let popup = egui::Popup::is_any_open(ctx);
            let (esc, enter, cmd_enter, cmd_e) = ui.input_mut(|i| {
                let cmd_enter = i.consume_key(egui::Modifiers::COMMAND, Key::Enter);
                (!popup && i.consume_key(egui::Modifiers::NONE, Key::Escape), !popup && i.consume_key(egui::Modifiers::NONE, Key::Enter), cmd_enter, i.consume_key(egui::Modifiers::COMMAND, Key::E))
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
                    // An empty database is the connection's default, worked out when it runs.
                    let label = if f.db.is_empty() { cname.clone() } else { format!("{cname} \u{b7} {}", f.db) };
                    // Built only while the menu is open, and scrolled, since a server can hold hundreds of databases.
                    let menu = ui.menu_button(RichText::new(label).color(BRIGHT()), |ui| {
                        menu_list(ui);
                        ui.add(egui::TextEdit::singleline(&mut f.target_q).hint_text("Filter databases").desired_width(240.0)).request_focus();
                        let tok = crate::app::sidebar::tokens(&f.target_q);
                        let hit = |n: &str| tok.iter().all(|w| n.to_lowercase().contains(w.as_str()));
                        let mut top = None;
                        egui::ScrollArea::vertical().max_height(LIST_MAX_H).show(ui, |ui| {
                            for c in self.snippet_conns(f.id) {
                                let Some(conn) = self.conn(c) else { continue };
                                // A connection whose name matches keeps all its databases.
                                let all = hit(&conn.meta.name);
                                let here = if Some(c) == f.conn { f.db.as_str() } else { "" };
                                let dbs: Vec<&str> = conn.dbs.iter().map(|d| d.0.as_str()).chain(conn.dbs.is_empty().then_some(here)).filter(|db| all || hit(db)).collect();
                                if dbs.is_empty() {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    ui.add_space(10.0);
                                    caption(ui, conn.meta.name.to_uppercase());
                                });
                                for db in dbs {
                                    top.get_or_insert((c, db));
                                    let text = if db.is_empty() { conn.meta.name.as_str() } else { db };
                                    menu_act(ui, &mut todo, Do::Target(c, db.to_string()), |ui| ui.selectable_label(Some(c) == f.conn && db == f.db, text));
                                }
                            }
                            if top.is_none() {
                                ui.label(RichText::new("No database matches").color(FAINT()));
                            }
                        });
                        if let Some((c, db)) = top
                            && ui.input(|i| i.key_pressed(Key::Enter))
                        {
                            todo.push(Do::Target(c, db.to_string()));
                            ui.close();
                        }
                    });
                    if menu.inner.is_none() {
                        f.target_q.clear();
                    }
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
                    egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(PREVIEW_MAX_H).show(ui, |ui| match &filled {
                        Ok(t) => {
                            // Laid out once per text, not every frame the form is open.
                            ui.label(match f.kind.as_str() {
                                "shell" => crate::ui::editor::memo_galley(ui, 13, t, ui.available_width(), || crate::ui::editor::js_job(t)),
                                "filter" => crate::ui::editor::memo_galley(ui, 14, t, ui.available_width(), || crate::ui::editor::json_job(t)),
                                _ => crate::ui::editor::memo_galley(ui, 9, t, ui.available_width(), || crate::ui::editor::sql_job_in(t, TXT_BODY, d)),
                            });
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
                if band_button(ui, band, "Unlock\u{2026}", WARNING()) || enter {
                    todo.push(Do::Unlock);
                }
            } else if bar == Bar::Confirm {
                let tag = if prod { ", a Production connection" } else { "" };
                let sub = if ready { format!("The {what} runs as written. Undo can't reverse it.") } else { format!("Waiting for {cname} to connect.") };
                band_lines(&pnt, band, &format!("Changes data on {cname}{tag}"), &sub);
                if (band_button(ui, band, &format!("Run {what}"), ALERT()) || cmd_enter) && ready {
                    todo.push(Do::Run);
                }
            } else {
                let run = if ready { "run" } else if failed { "connection failed: try again" } else if c.is_none() { "pick a connection first" } else { "connecting\u{2026}" };
                crate::app::palette::key_hints(&pnt, band.left() + 16.0, band.center().y, &[(&["tab"], "next value"), (&["\u{21b5}"], run)]);
                crate::app::palette::key_hint_right(&pnt, band.right() - 16.0, band.center().y, "esc", "back to search");
                if (enter || cmd_enter) && filled.is_ok() && ready {
                    todo.push(Do::Run);
                } else if enter && failed {
                    todo.push(Do::Retry);
                }
            }
            if esc {
                todo.push(Do::Back);
            }
            if cmd_e {
                todo.push(Do::Edit);
            }
        });
        // A click outside closes the form; Esc is taken above and steps back instead.
        if r.should_close() && !todo.iter().any(|t| matches!(t, Do::Back)) {
            return;
        }
        for t in todo {
            match t {
                Do::Back if f.stage == Stage::Confirm => f.stage = Stage::Fill,
                Do::Back => {
                    if let Some(query) = f.query.take() {
                        self.modal = Some(Modal::Palette { query, sel: 0, cmds: false, new_tab: false });
                    }
                    return;
                }
                Do::Retry => {
                    if let Some(id) = f.conn {
                        self.wake_conn(id);
                    }
                }
                Do::Target(c, db) => {
                    if Some(c) != f.conn {
                        f.schema = self.library.scripts.iter().find(|s| s.id == f.id && s.conn == c).map(|s| s.schema.clone()).unwrap_or_default();
                    }
                    f.conn = Some(c);
                    f.memo = None;
                    f.db = db;
                    f.stage = Stage::Fill;
                    self.wake_conn(c);
                }
                Do::MakeDefault => {
                    if let Some(id) = f.conn {
                        self.set_snippet_default(f.id, id, &f.db, &f.schema);
                    }
                }
                Do::Edit => return self.open_script(&ScriptRef::Saved(f.id), crate::app::library::open::How::Here),
                Do::Unlock => {
                    let Some(conn) = f.conn else { continue };
                    // Kept now, so "Stay read-only", which drops the form, does not drop what was typed.
                    self.lib_ui.snippet_values.insert(f.id, f.values.clone());
                    let mut back = f;
                    back.stage = Stage::Confirm;
                    self.ask_unlock_on(conn, move |a| a.modal = Some(Modal::Snippet(back)));
                    return;
                }
                Do::Run if writes && f.stage == Stage::Fill => f.stage = Stage::Confirm,
                Do::Run => {
                    let (Ok(text), Some(conn)) = (filled.clone(), f.conn) else { continue };
                    self.lib_ui.snippet_values.insert(f.id, f.values.clone());
                    // The database is worked out now that the connection is ready, as opening a script does.
                    if f.db.is_empty() {
                        f.db = self.default_db(conn);
                    }
                    if f.db.is_empty() && !self.is_pg(conn) {
                        self.warn("Pick a database to run this snippet in.");
                        continue;
                    }
                    return self.run_snippet(&f, conn, text);
                }
            }
        }
        if self.modal.is_none() {
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

    /// What the form runs, filled in: the statement or script, or for a filter its filter, sort and projection one per line.
    fn form_filled(&self, f: &SnippetForm, p: &snippet::Parsed, d: db::sql::Dialect) -> Result<String, String> {
        let l = form_lang(&f.kind, d);
        let bs = f.conn.is_none_or(|id| self.sql_bs_of(id));
        if f.kind != "filter" {
            return snippet::fill(&f.body, p, &f.values, l, bs);
        }
        let mut out = snippet::fill_named(&f.body, p, &f.values, l, bs)?;
        for (label, part) in [("sort", &f.sort), ("projection", &f.project)] {
            if !part.trim().is_empty() {
                out.push_str(&format!("\n// {label}\n{}", snippet::fill_named(part, p, &f.values, l, bs)?));
            }
        }
        Ok(out)
    }

    /// A Shell snippet runs in a new Shell tab on the form's target, a filter in the Documents view of its collection.
    /// Neither tab is bound to the script, and its text is already filled.
    fn run_mongo_snippet(&mut self, f: &SnippetForm, conn: u64, text: String) {
        let l = form_lang(&f.kind, Default::default());
        if f.kind == "shell" {
            if self.conn(conn).is_some_and(|c| c.meta.engine.is_sql()) {
                self.sql_new_query(conn, f.db.clone());
            } else {
                self.open_tab(Sel { conn, db: f.db.clone(), schema: String::new(), coll: String::new() }, Open::NewTab);
            }
            self.open_in_shell(text);
            if let Some(t) = self.tm() {
                (t.script, t.snip) = (None, Snip { off: true, ..Default::default() });
            }
            return self.start_shell();
        }
        let p = snippet::merge(f.parts().into_iter().map(|t| snippet::parse(t, l)));
        let fill = |s: &str| snippet::fill_named(s, &p, &f.values, l, true);
        let (filter, sort, project) = match (fill(&f.body), fill(&f.sort), fill(&f.project)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            (Err(e), ..) | (_, Err(e), _) | (.., Err(e)) => return self.warn(e),
        };
        self.open_tab(Sel { conn, db: f.db.clone(), schema: f.schema.clone(), coll: f.coll.clone() }, Open::Link);
        if let Some(t) = self.tm() {
            t.clear_options();
            (t.filter_txt, t.sort_txt, t.proj_txt, t.view) = (filter, sort, project, View::Documents);
            (t.script, t.snip) = (None, Snip { off: true, ..Default::default() });
        }
        self.run_query();
    }

    /// Runs the filled statement in a Query tab on the form's target: the current tab when it holds nothing of the
    /// person's on that connection and database, else a new one. The tab is not bound to the script: its text is the
    /// filled statement, and Cmd+S on it saves a new script.
    fn run_snippet(&mut self, f: &SnippetForm, conn: u64, text: String) {
        if f.kind != "sql" {
            return self.run_mongo_snippet(f, conn, text);
        }
        // Only a blank Query tab is taken, not one bound to a script, whose template would be lost.
        let reuse = self.tab_clean(self.cur, "sql") && self.t().is_some_and(|t| t.sel.conn == conn && t.sel.db == f.db && t.script.is_none());
        if !reuse {
            self.sql_new_query(conn, f.db.clone());
        }
        if let Some(t) = self.tm() {
            t.script = None;
            t.snip = Snip { off: true, ..Default::default() };
            if let Some(q) = t.sql.as_mut() {
                q.query = text;
                q.caret = 0;
                q.run_schema = f.schema.clone();
                q.run_db = String::new();
            }
        }
        self.sql_run(true, false);
    }
}

impl SnippetForm {
    /// The texts that hold values: a filter's sort and projection too.
    fn parts(&self) -> Vec<&str> {
        if self.kind == "filter" { vec![&self.body, &self.sort, &self.project] } else { vec![&self.body] }
    }
}

/// The language of a snippet of `kind`, SQL ones in the dialect of their connection.
fn form_lang(kind: &str, d: db::sql::Dialect) -> snippet::Lang {
    match kind {
        "shell" => snippet::Lang::Shell,
        "filter" => snippet::Lang::Query,
        _ => d.into(),
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

/// The action button at the right end of a bar.
fn band_button(ui: &mut egui::Ui, band: egui::Rect, label: &str, fill: Color32) -> bool {
    let at = band.shrink2(egui::vec2(16.0, 0.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(at).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| primary(ui, label, fill).clicked()).inner
}

/// What a write does, for "Run UPDATE": the first keyword of the statement the read-only check refuses, after its comments,
/// or after a WITH its first write.
fn verb(text: &str, d: db::sql::Dialect) -> String {
    use db::sql::lex::{read_only_check, split_statements, tokens, Tok};
    let stmt = split_statements(text, d).into_iter().map(|(_, s)| s).find(|s| read_only_check(s, d).is_err()).unwrap_or(text);
    let mut words = tokens(stmt, d).into_iter().filter(|(t, _)| *t == Tok::Word).map(|(_, r)| &stmt[r]);
    let is = |w: &str, k: &str| w.eq_ignore_ascii_case(k);
    match words.next() {
        Some(w) if is(w, "with") => words.find(|w| ["insert", "update", "delete", "merge"].iter().any(|k| is(w, k))),
        first => first,
    }
    .map_or_else(|| "statement".into(), str::to_ascii_uppercase)
}

impl App {
    /// QUARRY_TEST_SNIPPET=<state> over the MySQL demo: palette (Cmd+K finding two snippets), form (a read filled in),
    /// write (the confirm of a write), locked (a write on a locked connection), tab (a snippet open on Template), filled
    /// (the same on Filled) or make (Make it a value on a plain script).
    pub(in crate::app) fn demo_snippet(&mut self, state: &str) {
        use crate::library::Script;
        // Over the Mongo demo: shell (a Shell script with values) and docs (the Documents bar with values).
        if matches!(state, "shell" | "docs") {
            let Some(t) = self.tabs.get_mut(1) else { return };
            if state == "shell" {
                t.view = View::Shell;
                t.shell.code = "async function run() {\n  return await orders.find({\n    status: {{status:paid|shipped|refunded=paid}},\n    createdAt: { $gte: {{from:date}} },\n  }).limit({{n:int=20}}).toArray();\n}".into();
            } else {
                t.view = View::Documents;
                t.filter_txt = "{ status: {{status:paid|shipped=paid}}, total: { $gte: {{min:number=50}} } }".into();
                t.sort_txt = "{ createdAt: -1 }".into();
            }
            t.snip.vals.insert("from".into(), "2026-11-01".into());
            self.cur = 1;
            return;
        }
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
            "form" => self.open_snippet_form(read, Some("orders".into())),
            "write" | "locked" => {
                if state == "write"
                    && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == 3)
                {
                    c.writable = true;
                }
                self.open_snippet_form(write, Some("orders".into()));
                if let Some(Modal::Snippet(f)) = self.modal.as_mut()
                    && state == "write"
                {
                    f.stage = Stage::Confirm;
                }
            }
            "tab" | "filled" => {
                self.open_script(&ScriptRef::Saved(read), crate::app::library::open::How::Here);
                if let Some(t) = self.tm() {
                    t.snip.vals = [("day", "2026-11-03"), ("status", "paid"), ("limit", "100")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                    t.snip.filled = state == "filled";
                }
            }
            "make" => {
                let plain = add(self, "Orders of one day", "SELECT o.id, o.total FROM orders o WHERE DATE(o.created_at) = '2026-11-03';", reports);
                self.open_script(&ScriptRef::Saved(plain), crate::app::library::open::How::Here);
                if let Some(at) = self.t().and_then(|t| t.sql.as_ref()).and_then(|s| s.query.find("'2026-11-03'")) {
                    self.ask_make_value(at..at + 12, Src::Query);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::verb;
    use crate::db::sql::Dialect;

    #[test]
    fn the_confirm_names_the_write() {
        assert_eq!(verb("-- nightly fix\nUPDATE orders SET a = 1", Dialect::MySql), "UPDATE");
        assert_eq!(verb("WITH x AS (SELECT 1) DELETE FROM t", Dialect::Postgres), "DELETE");
        assert_eq!(verb("", Dialect::MySql), "statement");
        // The button names the statement that writes, not the first one.
        assert_eq!(verb("SELECT 1; DELETE FROM t", Dialect::MySql), "DELETE");
    }
}
