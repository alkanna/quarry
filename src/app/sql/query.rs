//! The Query view of a SQL tab: a script editor, Run, Stop and Explain, and one result tab per statement.

use crate::app::*;
use crate::db::sql::{lex, value::Kind, Col, ResultSet};
use db::copy::{CopyCol, Fmt};
use crate::ui::sql_complete::{open_list, sql_ac_field};
use egui::text::{CCursor, CCursorRange};
use std::ops::Range;
use std::sync::atomic::Ordering;

/// A fixed line height, so the gutter numbers and the statement band line up with the text.
const LINE_H: f32 = 20.0;

/// 1-based line and column of a caret given as a char index.
pub(in crate::app) fn line_col(text: &str, caret: usize) -> (usize, usize) {
    text.chars().take(caret).fold((1, 1), |(l, c), ch| if ch == '\n' { (l + 1, 1) } else { (l, c + 1) })
}

fn byte_at(text: &str, caret: usize) -> usize {
    text.char_indices().nth(caret).map_or(text.len(), |(b, _)| b)
}

/// Where the caret stands: line and column, and the statement under it.
#[derive(Clone)]
pub(in crate::app) struct CaretInfo {
    key: (usize, u64, usize),
    pub(in crate::app) line_col: (usize, usize),
    pub(in crate::app) stmt: Option<(Range<usize>, usize, usize)>,
}

impl SqlTab {
    /// The caret's place in the script. Lexing the script is costly next to a frame, so the answer is kept
    /// until the text or the caret changes.
    pub(in crate::app) fn caret_info(&self, d: db::sql::Dialect) -> CaretInfo {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        let key = (self.query.len(), h.finish(), self.caret);
        let mut slot = self.caret_cache.borrow_mut();
        match &*slot {
            Some(c) if c.key == key => c.clone(),
            _ => {
                let c = CaretInfo { key, line_col: line_col(&self.query, self.caret), stmt: statement_pos(&self.query, self.caret, d) };
                *slot = Some(c.clone());
                c
            }
        }
    }
}

/// The statement under a caret (a char index) as a byte range, with its 1-based number and the count of statements.
pub(in crate::app) fn statement_pos(text: &str, caret: usize, d: db::sql::Dialect) -> Option<(Range<usize>, usize, usize)> {
    let r = lex::statement_at(text, byte_at(text, caret), d)?;
    let all = lex::split_statements(text, d);
    let i = all.iter().position(|(a, _)| *a == r)?;
    Some((r, i + 1, all.len()))
}

/// The database a run uses: the picked one, else the tab's own.
fn run_db(t: &Tab) -> String {
    t.sql.as_ref().map(|s| s.run_db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| t.sel.db.clone())
}

/// The schema a PostgreSQL Query view runs in: the one typed in its schema field, else the tab's own when the run is in
/// the tab's own database. Another database has other schemas, so the tab's schema is never carried over to it and the
/// answer stays empty until that database's schemas arrive.
pub(in crate::app) fn run_schema_choice(typed: &str, run_db: &str, sel_db: &str, sel_schema: &str) -> String {
    let typed = typed.trim();
    if !typed.is_empty() {
        typed.to_string()
    } else if run_db.is_empty() || run_db == sel_db {
        sel_schema.to_string()
    } else {
        String::new()
    }
}

fn run_schema_of(t: &Tab) -> String {
    run_schema_choice(t.sql.as_ref().map_or("", |s| s.run_schema.as_str()), t.sql.as_ref().map_or("", |s| s.run_db.as_str()), &t.sel.db, &t.sel.schema)
}

/// The database a run connects to and the namespace inside it. A PostgreSQL connection is bound to one database, so
/// the `Db` points at it and the namespace is the tab's schema; MySQL picks the database per statement. Export of a
/// result uses it too, so the statement reads the same tables it ran against.
pub(in crate::app) fn run_target(t: &Tab, db: db::sql::Db) -> (db::sql::Db, String) {
    if db.dialect == db::sql::Dialect::Postgres {
        return (db.at(&run_db(t)), run_schema_of(t));
    }
    (db, run_db(t))
}

fn t_schema(t: Option<&Tab>) -> String {
    t.and_then(|t| t.sql.as_ref()).map(|s| s.run_schema.clone()).unwrap_or_default()
}

/// One result tab: the statement number, its kind and the outcome in color.
fn set_tab(ui: &mut egui::Ui, i: usize, set: &ResultSet, on: bool) -> egui::Response {
    let (detail, color) = if set.error.is_some() {
        (if set.refused { "refused: read-only" } else { "error" }.to_string(), ALERT())
    } else {
        let what = match set.affected {
            Some(n) => format!("{} affected", group(&n.to_string())),
            None => format!("{} {}", group(&set.rows.len().to_string()), if set.rows.len() == 1 { "row" } else { "rows" }),
        };
        (format!("{what} · {} ms", set.ms), if on { SUCCESS() } else { FAINT() })
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&(i + 1).to_string(), 0.0, mono_fmt(TXT_CAPTION, FAINT(), 400.0));
    job.append(&set.kind, 8.0, ui_fmt(TXT_SMALL, if on { BRIGHT() } else { MUTED() }, if on { W_MEDIUM } else { 400.0 }, 0.0));
    job.append(&detail, 8.0, mono_fmt(TXT_CAPTION, color, 400.0));
    let g = ui.painter().layout_job(job);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 20.0, BUTTON_H), Sense::click());
    let state = state_of(&r);
    ui.painter().rect(rect, R_MD, if on { PANEL() } else { hover_fill(state) }, Stroke::new(1.0, if on { BORDER() } else { Color32::TRANSPARENT }), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, optical_dy(&g)), g, TEXT());
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, "Result"));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// What the grid asks for: a row-menu action with its row, a check click (row and whether Shift extends a
/// range) and the header's select all.
#[derive(Default)]
struct GridOut {
    act: Option<(usize, DocsAct)>,
    check: Option<(usize, bool)>,
    all: Option<bool>,
}

/// The rows of one result set as a grid with a checkbox column. Checked rows are tinted, and the menu of a
/// checked row acts on every checked row.
fn grid(ui: &mut egui::Ui, set: &ResultSet, checked: &BTreeSet<usize>) -> GridOut {
    let mono = FontId::monospace(TXT_SMALL);
    let cw = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    // Widths from the headers and the first rows, so a wide cell further down is cut with an ellipsis.
    let wide = set.widest.get_or_init(|| {
        set.cols.iter().map(|c| set.rows.iter().take(60).filter_map(|r| r.value(&c.name)).map(|v| if let mongodb::bson::raw::RawBsonRef::String(s) = v { s.chars().count() + 2 } else { 10 }).max().unwrap_or(0)).collect()
    });
    let widths: Vec<f32> = set.cols.iter().zip(wide).map(|(c, &longest)| ((longest.max(c.name.chars().count()) as f32) * cw + 28.0).clamp(80.0, 360.0)).collect();
    let lead = CHECK_W + NUM_W;
    let total = lead + widths.iter().sum::<f32>();
    let mut out = GridOut::default();
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(1).show(ui, |ui| {
        let view_w = ui.available_width();
        let (head, _) = ui.allocate_exact_size(egui::vec2(view_w, TABLE_HEAD_H), Sense::hover());
        let area = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::both().id_salt("sql-grid").auto_shrink(false).show_rows(ui, ROW_H, set.rows.len(), |ui, range| {
                let rows = Rows::Raw(&set.rows);
                let shift = ui.input(|i| i.modifiers.shift);
                for i in range {
                    let (rr, resp) = ui.allocate_exact_size(egui::vec2(total.max(view_w - 2.0), ROW_H), Sense::click());
                    let hovered = ui.rect_contains_pointer(rr) && !ui.ctx().any_popup_open();
                    table::paint_row(ui, rr, false, checked.contains(&i), hovered, resp.is_pointer_button_down_on(), i % 2 == 1);
                    ui.painter().text(egui::pos2(rr.left() + CHECK_W + 18.0, rr.center().y), egui::Align2::LEFT_CENTER, (i + 1).to_string(), FontId::monospace(TXT_CAPTION), FAINT());
                    if check_box(ui, egui::pos2(rr.left() + 13.0, rr.center().y), ui.scope_id().with(("check", i)), checked.contains(&i), false).clicked() {
                        out.check = Some((i, shift));
                    }
                    let mut x = rr.left() + lead;
                    for (c, w) in set.cols.iter().zip(&widths) {
                        let cell = egui::Rect::from_min_size(egui::pos2(x, rr.top()), egui::vec2(*w, ROW_H));
                        x += w;
                        if !ui.is_rect_visible(cell) {
                            continue;
                        }
                        let (mut job, color) = match rows.value(i, &c.name) {
                            Some(v) => (sql_cell_job(v, c.kind, false), TEXT()),
                            None => (egui::text::LayoutJob::single_section("—".into(), mono_fmt(TXT_SMALL, FAINT(), 400.0)), FAINT()),
                        };
                        job.wrap = egui::text::TextWrapping::truncate_at_width(w - 14.0);
                        let g = ui.fonts_mut(|f| f.layout_job(job));
                        let gx = if is_numeric(c.kind) { cell.right() - 8.0 - g.size().x } else { cell.left() + 8.0 }.max(cell.left() + 8.0);
                        ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0))).galley(egui::pos2(gx, cell.center().y - g.size().y / 2.0), g, color);
                    }
                    // Cmd or Shift on a row checks it like in the Content grid.
                    if resp.clicked() && ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                        out.check = Some((i, shift));
                    }
                    resp.context_menu(|ui| {
                        menu_look(ui);
                        if let Some(a) = results_menu(ui, if checked.contains(&i) { checked.len() } else { 1 }, true, true) {
                            out.act = Some((i, a));
                            ui.close();
                        }
                    });
                }
            })
        });
        // Painted after the area so it follows the sideways scroll of that same frame.
        let off = area.inner.state.clamped_offset().x;
        let p = ui.painter().with_clip_rect(head);
        p.rect_filled(head, egui::CornerRadius { nw: R_MD - 1, ne: R_MD - 1, sw: 0, se: 0 }, HDR());
        let all = !set.rows.is_empty() && checked.len() == set.rows.len();
        if check_box(ui, egui::pos2(head.left() + 13.0 - off, head.center().y), ui.scope_id().with("check-all"), all, !all && !checked.is_empty()).on_hover_text("Select every row on this page").clicked() {
            out.all = Some(!all);
        }
        p.text(egui::pos2(head.left() + CHECK_W + 18.0 - off, head.center().y), egui::Align2::LEFT_CENTER, "#", FontId::monospace(TXT_CAPTION), FAINT());
        let mut x = head.left() + lead - off;
        for (c, w) in set.cols.iter().zip(&widths) {
            let g = p.layout_no_wrap(c.name.clone(), FontId::proportional(TXT_SMALL), MUTED());
            p.galley(egui::pos2(x + 8.0, head.center().y - g.size().y / 2.0 + optical_dy(&g)), g, MUTED());
            x += w;
        }
        p.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, LINE()));
    });
    out
}

/// Previous and next buttons for a result whose length is not known yet.
fn open_pager(ui: &mut egui::Ui, page: usize, next: bool) -> Option<usize> {
    let mut go = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        quiet_look(ui);
        let nav = |t: &str| quiet_button(RichText::new(t).font(FontId::monospace(TXT_BODY))).min_size(egui::vec2(22.0, BUTTON_H));
        if ui.add_enabled(page > 0, nav("«")).on_hover_text("First page").clicked() {
            go = Some(0);
        }
        if ui.add_enabled(page > 0, nav("‹")).on_hover_text("Previous page").clicked() {
            go = Some(page - 1);
        }
        ui.label(RichText::new(format!("Page {}", page + 1)).font(FontId::monospace(TXT_BODY)).color(MUTED()));
        if ui.add_enabled(next, nav("›")).on_hover_text("Next page").clicked() {
            go = Some(page + 1);
        }
    });
    go
}

impl App {
    /// The name a tab shows: its table, or "Query N" for a Query tab that belongs to no table.
    pub(in crate::app) fn tab_name(&self, i: usize) -> String {
        let Some(t) = self.tabs.get(i) else { return String::new() };
        if t.sql.is_some() && t.sel.coll.is_empty() {
            return format!("Query {}", self.tabs[..=i].iter().filter(|x| x.sql.is_some() && x.sel.coll.is_empty()).count());
        }
        t.sel.coll.clone()
    }

    /// Opens a Query tab for a database. It has no table, so its collection name stays empty.
    pub(in crate::app) fn sql_new_query(&mut self, conn: u64, db: String) {
        let engine = self.conn(conn).map(|c| c.meta.engine).unwrap_or_default();
        // A PostgreSQL Query tab starts in the schema the sidebar shows for its database.
        let mut t = Tab::new(self.sel_in(conn, &db, String::new())).for_engine(engine);
        // A database never expanded has no schema list yet; the tab fills its schema when the list arrives.
        if self.is_pg(conn) && !self.sql_asked.contains(&(conn, db.clone())) {
            self.pg_ask_schemas(conn, &db);
        }
        t.view = View::Sql;
        self.tabs.push(t);
        self.set_cur(self.tabs.len() - 1);
        self.reset_doc_state();
    }

    /// Runs the statement under the caret, or the whole script. `explain` puts EXPLAIN in front of the statement.
    pub(in crate::app) fn sql_run(&mut self, all: bool, explain: bool) {
        let Some(t) = self.t() else { return };
        let (conn, schema) = (t.sel.conn, run_db(t));
        let dialect = self.sql_dialect();
        let Some(s) = t.sql.as_ref().filter(|s| s.run_req == 0) else { return };
        let text = if all { s.query.clone() } else { s.caret_info(dialect).stmt.map(|(r, _, _)| s.query[r].to_string()).unwrap_or_default() };
        if text.trim().is_empty() {
            return self.warn("Nothing to run.");
        }
        let Some(db) = self.sql_db_for(conn, Some("Connect first.")) else { return };
        let (db, ns) = run_target(&self.tabs[self.cur], db);
        let read_only = !self.conn_writable(conn);
        let key = format!("{conn}/{schema}/#query");
        // History is written to ui.json and listed in the menu, so a password never goes in.
        push_history(&mut self.prefs.history.entry(key).or_default().selector, &db::redact_sql(&text).0);
        self.save_prefs();
        let text = if explain { format!("EXPLAIN {text}") } else { text };
        self.req += 1;
        let req = self.req;
        let Some(s) = self.tabs[self.cur].sql.as_mut() else { return };
        // Fresh handles per run, so a Stop pressed for an old run never reaches this one.
        (s.run_req, s.run) = (req, Default::default());
        self.worker.sql_run(req, db, ns, text, read_only, 0, s.run.clone());
    }

    /// Stops the running script: the statement in flight is interrupted and the rest never starts.
    fn sql_stop(&mut self) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref().filter(|s| s.run_req != 0) else { return };
        if let Some(db) = self.conn(t.sel.conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_kill(db, s.run.conn.load(Ordering::SeqCst), s.run.cancel.clone());
        }
    }

    /// Applies a finished run to its tab; a run the tab no longer waits for is dropped. A page read replaces
    /// only its result tab and keeps the row count; a new run starts the counts of its paged results.
    pub(in crate::app) fn sql_ran(&mut self, req: u64, mut sets: Vec<ResultSet>) {
        let Some(ti) = self.tabs.iter().position(|t| t.sql.as_ref().is_some_and(|s| s.run_req == req)) else { return };
        let conn = self.tabs[ti].sel.conn;
        let Some(s) = self.tabs[ti].sql.as_mut() else { return };
        // Checks belong to the page they were made on.
        (s.run_req, s.text_cache, s.qanchor) = (0, None, None);
        s.qchecked.clear();
        if let Some(idx) = s.paging.take() {
            let failed = match (s.sets.get_mut(idx), sets.pop()) {
                (Some(old), Some(mut new)) if new.error.is_none() => {
                    if let (Some(o), Some(n)) = (old.page, new.page.as_mut()) {
                        n.total = o.total;
                    }
                    *old = new;
                    None
                }
                (_, Some(new)) => new.error,
                _ => None,
            };
            if let Some(e) = failed {
                self.err(e);
            }
            return;
        }
        s.sets_req = req;
        (s.set_sel, s.sets) = (0, sets);
        // A last page already knows the total; the others ask the server.
        let wants: Vec<(usize, String)> = s.sets.iter().enumerate().filter(|(_, x)| x.page.is_some_and(|p| p.next)).map(|(i, x)| (i, x.stmt.clone())).collect();
        if let Some((db, ns)) = self.conn(conn).and_then(|c| c.sql.clone()).map(|db| run_target(&self.tabs[ti], db)) {
            for (i, stmt) in wants {
                self.worker.sql_count(req, i, db.clone(), ns.clone(), stmt);
            }
        }
    }

    /// The row count of a paged result arrived.
    pub(in crate::app) fn sql_counted(&mut self, req: u64, idx: usize, res: db::Res<u64>) {
        let p = self.tabs.iter_mut().find_map(|t| t.sql.as_mut().filter(|s| s.sets_req == req)).and_then(|s| s.sets.get_mut(idx)).and_then(|x| x.page.as_mut());
        if let (Some(p), Ok(n)) = (p, res) {
            p.total = Some(n);
        }
    }

    /// Reads result tab `idx` again at page `n` on a fresh connection, a run of its own that Stop reaches.
    fn sql_goto_page(&mut self, idx: usize, n: usize) {
        let Some(t) = self.t() else { return };
        let conn = t.sel.conn;
        let Some(stmt) = t.sql.as_ref().filter(|s| s.run_req == 0).and_then(|s| s.sets.get(idx)).map(|x| x.stmt.clone()) else { return };
        let Some(db) = self.sql_db_for(conn, Some("Connect first.")) else { return };
        let (db, schema) = run_target(&self.tabs[self.cur], db);
        let read_only = !self.conn_writable(conn);
        self.req += 1;
        let req = self.req;
        let Some(s) = self.tabs[self.cur].sql.as_mut() else { return };
        (s.run_req, s.paging, s.run) = (req, Some(idx), Default::default());
        self.worker.sql_run(req, db, schema, stmt, read_only, n, s.run.clone());
    }

    pub(in crate::app) fn sql_query_view(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        let (conn, free, schema) = (t.sel.conn, t.sel.coll.is_empty(), run_db(t));
        let running = t.sql.as_ref().is_some_and(|s| s.run_req != 0);
        let title = if free { self.tab_name(self.cur) } else { "Query".into() };
        let lib: Vec<SavedQuery> = library_of(&self.prefs.library, &Self::sql_lib_key(conn), LibKind::Sql);
        let (mut lib_new, mut lib_acts) = (std::mem::take(&mut self.new_lib), Vec::new());
        let (mut pick, mut load, mut picked) = (schema.clone(), None, false);
        let dialect = self.sql_dialect();
        let mut pg_schema = t_schema(self.t());
        let tab_schema = self.t().map(|t| t.sel.schema.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new(&title).size(TXT_HEADING).color(BRIGHT())));
            ui.add_space(6.0);
            ui.scope(|ui| {
                quiet_look(ui);
                egui::ComboBox::from_id_salt("sql-db").selected_text(RichText::new(&schema).font(FontId::monospace(TXT_SMALL))).show_ui(ui, |ui| {
                    menu_look(ui);
                    for (d, _) in self.conn(conn).map(|c| c.dbs.as_slice()).unwrap_or_default() {
                        picked |= ui.selectable_value(&mut pick, d.clone(), d).clicked();
                    }
                });
            });
            if dialect == db::sql::Dialect::Postgres {
                ui.add_space(4.0);
                ui.add(egui::TextEdit::singleline(&mut pg_schema).font(FontId::monospace(TXT_SMALL)).hint_text(RichText::new(&tab_schema).color(FAINT())).desired_width(100.0)).on_hover_text("The schema put first in search_path for this tab. Empty uses the schema this tab was opened in.");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The first one added sits at the right edge: the history, then the library beside it.
                icon_menu(ui, paint_clock, true, |ui| {
                    ui.set_min_width(360.0);
                    ui.set_max_width(520.0);
                    let hist = self.prefs.history.get(&format!("{conn}/{schema}/#query")).map(|h| h.selector.as_slice()).unwrap_or_default();
                    if hist.is_empty() {
                        ui.label(RichText::new("Statements you run appear here.").color(MUTED()));
                    }
                    egui::ScrollArea::vertical().id_salt("sql-hist").max_height(300.0).show(ui, |ui| {
                        for h in hist {
                            let one_line: String = h.split_whitespace().collect::<Vec<_>>().join(" ");
                            let mut job = sql_job(&one_line.chars().take(90).collect::<String>(), TXT_SMALL);
                            job.wrap.max_rows = 1;
                            if ui.add(quiet_button(job)).on_hover_text(h.as_str()).clicked() {
                                load = Some(h.clone());
                                ui.close();
                            }
                        }
                    });
                })
                .on_hover_text("History");
                ui.add_space(6.0);
                let button = library_menu(ui, &lib, &mut lib_new, LibMenu::Sql(dialect), false, &mut lib_acts);
                self.demo_open_library(ui.ctx(), &button);
            });
        });
        self.new_lib = lib_new;
        if let Some(s) = self.tm().and_then(|t| t.sql.as_mut()) {
            // Kept as typed: trimming here would eat a space the user is still typing. Every reader trims.
            s.run_schema = pg_schema;
        }
        ui.add_space(4.0);
        let (mut run, mut run_all, mut stop, mut explain, mut format) = (false, false, false, false, false);
        let status = self.t().and_then(|t| t.sql.as_ref()).map(|s| {
            let CaretInfo { line_col: (l, c), stmt, .. } = s.caret_info(self.sql_dialect());
            let n = stmt.map_or(String::new(), |(_, i, n)| format!("  ·  statement {i} of {n}"));
            format!("Ln {l}, Col {c}{n}")
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_enabled_ui(!running, |ui| {
                run = run_button(ui, "Run statement", "⌘↵").on_hover_text("Run the statement under the caret (Cmd+Enter)").clicked();
                run_all = ui.add(egui::Button::new("Run all").right_text(RichText::new("⇧⌘↵").size(TXT_CAPTION).color(FAINT()))).on_hover_text("Run every statement (Cmd+Shift+Enter)").clicked();
            });
            stop = quiet_enabled(ui, running, "Stop").on_hover_text("Interrupt the running statement and skip the rest").clicked();
            let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), Sense::hover());
            ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, LINE()));
            explain = quiet_enabled(ui, !running, "Explain").on_hover_text("EXPLAIN the statement under the caret (Cmd+E)").clicked();
            format = quiet_enabled(ui, !running, "Format").on_hover_text("Lay out the selection, or the whole script when nothing is selected. A statement that would read differently stays as it is.").clicked();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(s) = &status {
                    ui.label(RichText::new(s).font(FontId::monospace(TXT_CAPTION)).color(FAINT()));
                }
            });
        });
        ui.add_space(8.0);
        if picked {
            // Another PostgreSQL database has its own schemas, so the tab moves to the one its sidebar shows.
            let schema = self.ns_of(conn, &pick).to_string();
            if self.is_pg(conn) && !self.sql_asked.contains(&(conn, pick.clone())) {
                self.pg_ask_schemas(conn, &pick);
            }
            if let Some(t) = self.tm() {
                match (free, t.sql.as_mut()) {
                    (true, _) => (t.sel.db, t.sel.schema) = (pick, schema),
                    (false, Some(s)) => (s.run_db, s.run_schema) = (pick, schema),
                    _ => {}
                }
            }
        }
        let ctx = ui.ctx().clone();
        if let Some(text) = load {
            self.sql_replace(&ctx, text, None);
        }
        for a in lib_acts {
            match a {
                QAct::Load(q) => self.sql_replace(&ctx, q.sql.unwrap_or_default(), None),
                QAct::Save(name) => {
                    let sql = self.t().and_then(|t| t.sql.as_ref()).map(|s| s.query.clone()).unwrap_or_default();
                    if sql.trim().is_empty() {
                        self.warn("Nothing to save: the script is empty.");
                    } else {
                        let has_password = !db::redact_sql(&sql).1.is_empty();
                        self.save_to_library(SavedQuery { name, sql: Some(sql), ..Default::default() });
                        // Saved as typed, because a script with its password cut out would not run.
                        if has_password {
                            self.warn("Saved, but the script holds a password in plain text in the library file.");
                        }
                    }
                }
                QAct::Delete(name) => self.delete_sql_from_library(&name),
                _ => {}
            }
        }
        if format {
            self.sql_format(&ctx);
        }
        self.sql_editor(ui);
        ui.add_space(4.0);
        self.sql_result_pane(ui, running);
        if stop {
            self.sql_stop();
        }
        if run || run_all || explain {
            self.sql_run(run_all, explain);
        }
    }

    pub(in crate::app) fn sql_editor_id(cur: usize) -> egui::Id {
        egui::Id::unique(("sql-editor", cur))
    }

    /// Puts `new` in the editor so that Cmd+Z brings the old text back. egui's editor keeps its undo history as whole
    /// states, so the text before the change is added as a point first; changing the string alone would only become
    /// one after the text had rested for a second. `select` is the range of the new text to leave selected.
    pub(in crate::app) fn sql_replace(&mut self, ctx: &egui::Context, new: String, select: Option<Range<usize>>) {
        let id = Self::sql_editor_id(self.cur);
        let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) else { return };
        if s.query == new {
            return;
        }
        let mut st = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let before = st.cursor.char_range().unwrap_or_else(|| CCursorRange::one(CCursor::new(s.query.chars().count())));
        let mut undoer = st.undoer();
        undoer.add_undo(&(before, std::mem::replace(&mut s.query, new)));
        st.set_undoer(undoer);
        let end = s.query.chars().count();
        let range = match select {
            Some(r) => CCursorRange::two(CCursor::new(r.start.min(end)), CCursor::new(r.end.min(end))),
            None => CCursorRange::one(CCursor::new(usize::from(before.primary.index).min(end))),
        };
        st.cursor.set_char_range(Some(range));
        s.caret = usize::from(range.primary.index);
        st.store(ctx, id);
        ctx.request_repaint();
    }

    /// Format: the selection, or the whole script when nothing is selected.
    fn sql_format(&mut self, ctx: &egui::Context) {
        let Some(s) = self.tabs.get(self.cur).and_then(|t| t.sql.as_ref()) else { return };
        let text = s.query.clone();
        let picked = egui::TextEdit::load_state(ctx, Self::sql_editor_id(self.cur)).and_then(|st| st.cursor.char_range()).filter(|r| r.primary != r.secondary);
        let (new, select, f) = match picked {
            None => {
                let f = db::sql::format::format_script(&text, self.sql_dialect());
                (f.text.clone(), None, f)
            }
            Some(r) => {
                let (p, q) = (usize::from(r.primary.index), usize::from(r.secondary.index));
                let (a, b) = (p.min(q), p.max(q));
                let (a, b) = (byte_at(&text, a), byte_at(&text, b));
                // A cut through a string or comment would be formatted as something it is not.
                if lex::tokens(&text, self.sql_dialect()).iter().any(|(t, tr)| matches!(t, lex::Tok::Str | lex::Tok::Quoted | lex::Tok::Comment) && ((tr.start < a && a < tr.end) || (tr.start < b && b < tr.end))) {
                    return self.warn("The selection starts or ends inside a string or comment. Select whole statements, or nothing to format the script.");
                }
                let f = db::sql::format::format_script(&text[a..b], self.sql_dialect());
                let new = format!("{}{}{}", &text[..a], f.text, &text[b..]);
                let from = text[..a].chars().count();
                (new, Some(from..from + f.text.chars().count()), f)
            }
        };
        let note = if new == text && f.special == 0 && f.refused == 0 { "Already formatted".to_string() } else { f.summary() };
        self.sql_replace(ctx, new, select);
        if f.refused > 0 { self.warn(note) } else { self.ok(note) }
    }

    /// The script editor with line numbers, token colors and a faint band behind the statement under the caret.
    fn sql_editor(&mut self, ui: &mut egui::Ui) {
        let id = Self::sql_editor_id(self.cur);
        let focused = ui.memory(|m| m.has_focus(id));
        let avail = ui.available_height();
        let Some((conn, db, schema)) = self.tabs.get(self.cur).map(|t| (t.sel.conn, run_db(t), if self.is_pg(t.sel.conn) { Some(run_schema_of(t)).filter(|s| !s.is_empty()).unwrap_or_else(|| self.sel_schema(&t.sel)) } else { String::new() })) else { return };
        let scope = Self::sql_scope(&self.conns, &self.sql_schema, &self.views, conn, &db, &schema, None);
        let dialect = scope.dialect;
        let none = Fields::new();
        let values = self.sql_values.get(&(conn, ns_key(&db, &schema))).unwrap_or(&none);
        let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) else { return };
        let lines = s.query.split('\n').count();
        // The editor keeps three lines, and the results below it their tabs, header and three rows.
        let (lo, hi) = (3.0 * LINE_H + 24.0, avail - 3.0 * ROW_H - TABLE_HEAD_H - 110.0);
        let custom = self.prefs.query_editor_h > 0.0;
        let h = if custom { self.prefs.query_editor_h.clamp(lo, hi.max(lo)) } else { (lines as f32 * LINE_H + 24.0).clamp(96.0, (avail * 0.5).max(96.0)) };
        let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            crate::ui::editor::memo_galley(ui, 1, buf.as_str(), wrap, || {
                let mut job = crate::ui::editor::sql_job_in(buf.as_str(), TXT_SMALL, dialect);
                for sec in &mut job.sections {
                    sec.format.line_height = Some(LINE_H);
                }
                job
            })
        };
        let framed = egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("sql-ed").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                // Reserved first so the band is drawn under the text.
                let band = ui.painter().add(egui::Shape::Noop);
                if s.bar.preset.ac {
                    open_list(ui, id, &s.query, &mut s.bar.ac, &scope, values);
                }
                // The suggestions work on the statement under the caret, with the tail after it for aliases.
                let (out, _) = sql_ac_field(ui, id, &mut s.query, &mut s.bar.ac, &scope, values, &mut s.bar.wants, |ui, text, _| {
                    egui::TextEdit::multiline(text)
                        .id(id)
                        .font(FontId::monospace(TXT_SMALL))
                        .frame(gutter_frame())
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, if custom { h } else { 96.0 }))
                        .lock_focus(true)
                        .hint_text(RichText::new("SELECT * FROM orders;").font(FontId::monospace(TXT_SMALL)).color(FAINT()).line_height(Some(LINE_H)))
                        .layouter(&mut layout)
                        .show(ui)
                });
                if let Some(r) = out.cursor_range {
                    s.caret = usize::from(r.primary.index);
                }
                let (g, gp, rect) = (&out.galley, out.galley_pos, out.response.rect);
                let info = s.caret_info(dialect);
                if let Some((r, _, _)) = info.stmt.filter(|p| p.2 > 1) {
                    let at = |b: usize| s.query[..b].chars().count();
                    let (a, z) = (g.pos_from_cursor(CCursor::new(at(r.start))), g.pos_from_cursor(CCursor::new(at(r.end))));
                    let band_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), gp.y + a.top()), egui::pos2(rect.right(), gp.y + z.bottom()));
                    ui.painter().set(band, egui::Shape::rect_filled(band_rect, 0.0, tint(PRIMARY(), 22)));
                }
                paint_gutter(ui, &out, Some(info.line_col.0));
            });
        });
        let wants = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()).map(|s| std::mem::take(&mut s.bar.wants)).unwrap_or_default();
        self.sql_serve(conn, &db, &schema, wants);
        let bar = splitter(ui);
        if resize_editor(&bar, &mut self.prefs.query_editor_h, framed.response.rect.height(), lo, hi) {
            self.save_prefs();
        }
    }

    /// The result tabs, the Table or Text switch and the selected statement's outcome.
    fn sql_result_pane(&mut self, ui: &mut egui::Ui, running: bool) {
        let cur = self.cur;
        let hidden = self.modal.is_some() || self.insert.is_some();
        let Some(s) = self.tabs.get_mut(cur).and_then(|t| t.sql.as_mut()) else { return };
        if s.sets.is_empty() {
            ui.horizontal(|ui| {
                if running {
                    spin(ui);
                }
                ui.label(RichText::new(if running { "Running…" } else { "Run a statement to see its result." }).color(MUTED()));
            });
            return;
        }
        let (mut pick, mut text_mode) = (None, s.text_mode);
        // What the frame asked for, done after `s` is let go: rows to copy (None is the whole page), rows to
        // export with whether the dialog starts on every page, copying every page, and a page to read.
        let (mut copy, mut export, mut copy_all, mut goto): (Option<(Fmt, Option<Vec<usize>>)>, Option<(Vec<usize>, bool)>, Option<Fmt>, Option<(usize, usize)>) = (None, None, None, None);
        let sel = s.set_sel.min(s.sets.len() - 1);
        let paged = s.sets[sel].page.filter(|p| p.next || p.n > 0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let w = (ui.available_width() - 260.0).max(120.0);
            ui.allocate_ui(egui::vec2(w, BUTTON_H + 8.0), |ui| {
                egui::ScrollArea::horizontal().id_salt("sql-sets").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (i, set) in s.sets.iter().enumerate() {
                            if set_tab(ui, i, set, i == sel).clicked() {
                                pick = Some(i);
                            }
                        }
                        if running {
                            spin(ui);
                        }
                    });
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                dots_menu(ui, |ui| {
                    menu_look(ui);
                    if let Some(DocsAct::CopyAs(f)) = ui.menu_button("Copy as", |ui| copy_as_menu(ui, true)).inner.flatten() {
                        copy = Some((f, None));
                        ui.close();
                    }
                    if paged.is_some() {
                        if let Some(DocsAct::CopyAs(f)) = ui.menu_button("Copy all pages as", |ui| copy_as_menu(ui, true)).inner.flatten() {
                            copy_all = Some(f);
                            ui.close();
                        }
                    }
                    if ui.button("Export…").clicked() {
                        export = Some((s.qchecked.iter().copied().collect(), false));
                        ui.close();
                    }
                });
                if quiet(ui, "Export…").on_hover_text("Write this result to a file in any Copy as format").clicked() {
                    export = Some((s.qchecked.iter().copied().collect(), false));
                }
                segmented(ui, &mut text_mode, &[(false, "Table"), (true, "Text")]);
            });
        });
        ui.add_space(6.0);
        s.text_mode = text_mode;
        if let Some(i) = pick {
            (s.set_sel, s.text_cache) = (i, None);
            s.qchecked.clear();
            s.qanchor = None;
        }
        let (error, stmt, affected, ms, capped, page) = {
            let x = &s.sets[sel];
            (x.error.clone(), x.stmt.clone(), x.affected, x.ms, x.capped, x.page)
        };
        // The page bar: how many rows there are, the pager and a note that unordered rows can move.
        if let (Some(p), None) = (page, &error) {
            let known = p.total.or_else(|| (!p.next).then(|| (p.n * db::sql::RUN_ROWS_MAX + s.sets[sel].rows.len()) as u64));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let count = match known {
                    Some(n) => format!("{} {}", group(&n.to_string()), if n == 1 { "row" } else { "rows" }),
                    None => format!("{}+ rows", group(&((p.n + 1) * db::sql::RUN_ROWS_MAX).to_string())),
                };
                ui.label(semibold(RichText::new(count).font(FontId::monospace(TXT_SMALL)).color(BRIGHT())));
                if !p.ordered && (p.next || p.n > 0) {
                    ui.label(RichText::new("No ORDER BY: rows may move between pages").size(TXT_CAPTION).color(FAINT()));
                }
                if p.next || p.n > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The pager reads left to right, so it gets its own layout at last frame's width, as in Content.
                        let id = ui.scope_id().with("pager-w");
                        let w = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(150.0);
                        let r = ui.allocate_ui_with_layout(egui::vec2(w, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| match known {
                            Some(n) => pager(ui, p.n, (n as usize).div_ceil(db::sql::RUN_ROWS_MAX).max(1)),
                            None => open_pager(ui, p.n, p.next),
                        });
                        if r.response.rect.width() != w {
                            ui.data_mut(|d| d.insert_temp(id, r.response.rect.width()));
                        }
                        goto = r.inner.filter(|n| *n != p.n && !running).map(|n| (sel, n));
                    });
                }
            });
            ui.add_space(4.0);
        }
        if let Some(e) = &error {
            egui::Frame::new().fill(tint(ALERT(), 22)).stroke(Stroke::new(1.0, tint(ALERT(), 90))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&stmt).font(FontId::monospace(TXT_SMALL)).color(MUTED()));
                ui.label(RichText::new(e).color(ALERT()));
            });
        } else if let Some(n) = affected {
            ui.label(RichText::new(format!("{} {} affected in {} ms.", group(&n.to_string()), if n == 1 { "row" } else { "rows" }, ms)).color(MUTED()));
        } else {
            if capped {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new(format!("Showing the first {} rows. Export all to get every row.", group(&db::sql::RUN_ROWS_MAX.to_string()))).font(FontId::monospace(TXT_SMALL)).color(WARNING()));
                    if quiet(ui, "Export all…").on_hover_text("Run the statement again and stream every row to a file").clicked() {
                        export = Some((s.qchecked.iter().copied().collect(), true));
                    }
                });
                ui.add_space(4.0);
            }
            let checked_n = s.qchecked.len();
            if s.sets[sel].rows.is_empty() {
                ui.label(RichText::new("No rows").color(MUTED()));
            } else if text_mode {
                if s.text_cache.as_ref().is_none_or(|c| c.0 != sel) {
                    let docs: Vec<Document> = s.sets[sel].rows.iter().map(db::Row::owned).collect();
                    s.text_cache = Some((sel, db::copy::text_table(&s.sets[sel].cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>(), &docs)));
                }
                let text = s.text_cache.as_ref().map(|c| c.1.as_str()).unwrap_or_default();
                egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
                    egui::ScrollArea::both().id_salt("sql-text").auto_shrink(false).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text).font(FontId::monospace(TXT_SMALL)).color(TEXT())).extend());
                    });
                });
            } else {
                // The bar floats over the bottom of the grid, so it is placed before the grid takes the room.
                if !hidden {
                    let (cancel, act) = selection_area(ui, checked_n, false, true, true, false);
                    if cancel {
                        s.qchecked.clear();
                    }
                    match act {
                        Some(DocsAct::CopyAs(f)) => copy = Some((f, Some(s.qchecked.iter().copied().collect()))),
                        Some(DocsAct::Export) => export = Some((s.qchecked.iter().copied().collect(), false)),
                        _ => {}
                    }
                }
                let out = grid(ui, &s.sets[sel], &s.qchecked);
                if let Some(on) = out.all {
                    s.qchecked = if on { (0..s.sets[sel].rows.len()).collect() } else { BTreeSet::new() };
                }
                if let Some((i, a)) = out.act {
                    let targets: Vec<usize> = if s.qchecked.contains(&i) { s.qchecked.iter().copied().collect() } else { vec![i] };
                    match a {
                        DocsAct::CopyAs(f) => copy = Some((f, Some(targets))),
                        DocsAct::Export => export = Some((targets, false)),
                        _ => {}
                    }
                }
                if let Some((i, range)) = out.check {
                    match s.qanchor.filter(|_| range) {
                        Some(a) => s.qchecked.extend(a.min(i)..=a.max(i)),
                        None => {
                            if !s.qchecked.insert(i) {
                                s.qchecked.remove(&i);
                            }
                        }
                    }
                    s.qanchor = Some(i);
                }
            }
        }
        let tab_coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        let set_rows = |app: &App, idx: &Option<Vec<usize>>| -> Vec<Document> {
            let Some(s) = app.t().and_then(|t| t.sql.as_ref()) else { return Vec::new() };
            let rows = &s.sets[sel.min(s.sets.len().saturating_sub(1))].rows;
            rows.iter().enumerate().filter(|(i, _)| idx.as_ref().is_none_or(|v| v.contains(i))).map(|(_, r)| r.owned()).collect()
        };
        if let Some((fmt, idx)) = copy {
            let rows = set_rows(self, &idx);
            let cols: Vec<CopyCol> = self.tabs[cur].sql.as_ref().map(|s| s.sets[sel].cols.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: is_numeric(c.kind) }).collect()).unwrap_or_default();
            let table = if tab_coll.is_empty() { "result".to_string() } else { tab_coll };
            let ctx = ui.ctx().clone();
            self.copy_sheet(&ctx, fmt, &rows, &cols, &table, &[], self.sql_bs());
        }
        if let Some((idx, all)) = export {
            let rows = set_rows(self, &Some(idx));
            self.open_export(rows, all);
        }
        if let Some(fmt) = copy_all {
            let header = ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::unique("copy-with-header"))).unwrap_or(true);
            self.copy_all_pages(fmt, header);
        }
        if let Some((idx, n)) = goto {
            self.sql_goto_page(idx, n);
        }
    }

    /// QUARRY_TEST_SQL=query: a Query tab with the multi-statement script of the design and its three outcomes.
    pub(in crate::app) fn sql_query_demo(&mut self) {
        let mut t = Tab::new(Sel { conn: 3, db: "shop".into(), coll: String::new(), ..Default::default() }).for_engine(db::sql::Engine::MySql);
        t.view = View::Sql;
        let col = |name: &str, ty: &str| Col::plain(name, ty, Kind::from_type_name(ty), false);
        let people: [(&str, &str, i64, &str); 7] = [
            ("Ada Byron", "ada@example.com", 6, "1412.40"),
            ("Grace Hopper", "grace@example.com", 5, "988.15"),
            ("Ken Thompson", "ken@example.com", 4, "712.00"),
            ("Barbara Liskov", "barbara@example.com", 4, "540.75"),
            ("Edsger Dijkstra", "edsger@example.com", 3, "301.20"),
            ("Margaret Hamilton", "margaret@example.com", 3, "288.00"),
            ("Dennis Ritchie", "dennis@example.com", 2, "190.50"),
        ];
        let raw = "-- Paid orders over the last week, by customer\nSELECT c.name, c.email, COUNT(*) AS orders, SUM(o.total) AS spent\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'paid'\n  AND o.created_at >= CURDATE() - INTERVAL 7 DAY\nGROUP BY c.id\nORDER BY spent DESC;\n\nUPDATE orders SET status = 'shipped' WHERE id IN (4405, 4407);\nSELECT * FROM shipments WHERE order_id = 4405;";
        // QUARRY_TEST_SQL=format shows the same script after Format.
        // QUARRY_TEST_SQL=library has two saved scripts and the menu open.
        if std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("library") {
            let saved = |name: &str, sql: &str| SavedQuery { name: name.into(), sql: Some(sql.into()), ..Default::default() };
            self.prefs.library.insert(Self::sql_lib_key(3), vec![saved("Late shipments", "SELECT o.id, o.created_at, s.carrier\nFROM orders o\nLEFT JOIN shipments s ON s.order_id = o.id\nWHERE o.status = 'paid' AND s.id IS NULL;"), saved("Revenue by tier", "SELECT tier, SUM(total) AS revenue FROM orders GROUP BY tier ORDER BY revenue DESC;")]);
        }
        let query = if std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("format") { db::sql::format::format_script(raw, db::sql::Dialect::MySql).text } else { raw.to_string() };
        let query = query.as_str();
        let stmt = query.lines().skip(1).take(7).collect::<Vec<_>>().join("\n");
        let first = ResultSet {
            cols: vec![col("name", "VARCHAR"), col("email", "VARCHAR"), col("orders", "BIGINT"), col("spent", "DECIMAL")],
            rows: people.iter().filter_map(|&(n, e, o, s)| db::Row::from_doc(&doc! { "name": n, "email": e, "orders": o, "spent": s }).ok()).collect(),
            ms: 31,
            ..ResultSet::new(&stmt)
        };
        let refused = ResultSet { error: Some("This statement is not allowed on a read-only connection. Unlock writes to run it.".into()), refused: true, ..ResultSet::new("UPDATE orders SET status = 'shipped' WHERE id IN (4405, 4407)") };
        let stmt = "SELECT * FROM shipments WHERE order_id = 4405";
        let last = ResultSet {
            cols: vec![col("id", "INT"), col("order_id", "INT"), col("carrier", "VARCHAR")],
            rows: db::Row::from_doc(&doc! { "id": 912_i64, "order_id": 4405_i64, "carrier": "DHL" }).into_iter().collect(),
            ms: 4,
            ..ResultSet::new(stmt)
        };
        if let Some(s) = t.sql.as_mut() {
            s.caret = query.lines().take(6).map(|l| l.chars().count() + 1).sum::<usize>() - 1;
            (s.query, s.sets) = (query.into(), vec![first, refused, last]);
            // QUARRY_TEST_CHECKED=1 shows three rows checked with the selection bar.
            if std::env::var("QUARRY_TEST_CHECKED").is_ok() {
                s.qchecked = [0, 2, 3].into();
            }
        }
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
        // QUARRY_TEST_EXPORT=1 opens the Export dialog on the checked rows.
        // QUARRY_TEST_EXPORT=script shows the scope "All statements in the editor" on the demo script, which has a write in it
        // and so is refused; =script-ok shows it on a script of reads.
        if let Ok(v) = std::env::var("QUARRY_TEST_EXPORT") {
            let rows = self.checked_rows();
            self.open_export(rows, false);
            match v.as_str() {
                "script" => self.demo_script_scope(None),
                "script-ok" => self.demo_script_scope(Some("USE shop;\nSET @since = '2024-01-01';\nSELECT id, status, total FROM orders WHERE created_at > @since;\nSELECT carrier, COUNT(*) AS n FROM shipments GROUP BY carrier")),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_database_never_inherits_the_tabs_schema() {
        // Typed wins, and the tab's own schema serves only its own database.
        assert_eq!(run_schema_choice(" audit ", "b", "a", "public"), "audit");
        assert_eq!(run_schema_choice("sales data ", "b", "a", "public"), "sales data");
        assert_eq!(run_schema_choice("  ", "a", "a", "public"), "public");
        assert_eq!(run_schema_choice("", "", "a", "public"), "public");
        assert_eq!(run_schema_choice("", "a", "a", "public"), "public");
        assert_eq!(run_schema_choice("", "b", "a", "public"), "");
    }

    #[test]
    fn caret_line_and_column() {
        let t = "SELECT 1;\n  SELECT é;";
        assert_eq!(line_col(t, 0), (1, 1));
        assert_eq!(line_col(t, 10), (2, 1));
        // The caret counts chars, not bytes.
        assert_eq!(line_col(t, t.chars().count()), (2, 12));
    }

    #[test]
    fn statement_under_the_caret() {
        const D: db::sql::Dialect = db::sql::Dialect::MySql;
        let t = "SELECT 1;\n\nUPDATE a SET b = 2;\nSELECT 'x;y';";
        let (r, i, n) = statement_pos(t, 3, D).unwrap();
        assert_eq!((&t[r], i, n), ("SELECT 1", 1, 3));
        let (_, i, _) = statement_pos(t, 14, D).unwrap();
        assert_eq!(i, 2);
        // A caret past the end stays with the last statement.
        assert_eq!(statement_pos(t, 999, D).unwrap().1, 3);
        assert!(statement_pos("  ", 0, D).is_none());
    }

    #[test]
    fn kind_skips_comments() {
        assert_eq!(ResultSet::new("-- hi\n  select 1").kind, "SELECT");
        assert_eq!(ResultSet::new("").kind, "");
    }
}
