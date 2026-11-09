//! The Query view of a SQL tab: a script editor, Run, Stop and Explain, and one result tab per statement.

use super::*;
use crate::db::sql::{lex, value::Kind, Col, ResultSet};
use db::copy::{CopyCol, Fmt};
use super::sql_bar::sql_job;
use egui::text::CCursor;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A fixed line height, so the gutter numbers and the statement band line up with the text.
const LINE_H: f32 = 20.0;
const GUTTER_W: f32 = 52.0;

/// 1-based line and column of a caret given as a char index.
pub(super) fn line_col(text: &str, caret: usize) -> (usize, usize) {
    text.chars().take(caret).fold((1, 1), |(l, c), ch| if ch == '\n' { (l + 1, 1) } else { (l, c + 1) })
}

fn byte_at(text: &str, caret: usize) -> usize {
    text.char_indices().nth(caret).map_or(text.len(), |(b, _)| b)
}

/// The statement under a caret (a char index) as a byte range, with its 1-based number and the count of statements.
pub(super) fn statement_pos(text: &str, caret: usize) -> Option<(Range<usize>, usize, usize)> {
    let r = lex::statement_at(text, byte_at(text, caret))?;
    let all = lex::split_statements(text);
    let i = all.iter().position(|(a, _)| *a == r)?;
    Some((r, i + 1, all.len()))
}

/// The first keyword of a statement in capitals, for the result tab ("SELECT", "UPDATE").
fn stmt_kind(stmt: &str) -> String {
    lex::tokens(stmt).into_iter().find(|(t, _)| *t == lex::Tok::Word).map(|(_, r)| stmt[r].to_ascii_uppercase()).unwrap_or_default()
}

/// The database a run uses: the picked one, else the tab's own.
fn run_db(t: &Tab) -> String {
    t.sql.as_ref().map(|s| s.run_db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| t.sel.db.clone())
}

/// One result tab: the statement number, its kind and the outcome in color.
fn set_tab(ui: &mut egui::Ui, i: usize, set: &ResultSet, on: bool, read_only: bool) -> egui::Response {
    let (detail, color) = if let Some(e) = &set.error {
        (if read_only && (e.contains("read-only") || e.contains("not allowed")) { "refused: read-only".to_string() } else { "error".to_string() }, ALERT())
    } else {
        let what = match set.affected {
            Some(n) => format!("{} affected", group(&n.to_string())),
            None => format!("{} {}", group(&set.rows.len().to_string()), if set.rows.len() == 1 { "row" } else { "rows" }),
        };
        (format!("{what} · {} ms", set.ms), if on { SUCCESS() } else { FAINT() })
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&(i + 1).to_string(), 0.0, mono_fmt(TXT_CAPTION, FAINT(), 400.0));
    job.append(&stmt_kind(&set.stmt), 8.0, ui_fmt(TXT_SMALL, if on { BRIGHT() } else { MUTED() }, if on { W_MEDIUM } else { 400.0 }, 0.0));
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

/// The rows of one result set as a read-only grid. Returns the Copy as request from a row menu: the format and the row.
fn grid(ui: &mut egui::Ui, set: &ResultSet) -> Option<(Fmt, usize)> {
    let mono = FontId::monospace(TXT_SMALL);
    let cw = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    // Widths from the headers and the first rows, so a wide cell further down is cut with an ellipsis.
    let widths: Vec<f32> = set
        .cols
        .iter()
        .map(|c| {
            let longest = set.rows.iter().take(60).filter_map(|r| r.value(&c.name)).map(|v| if let mongodb::bson::raw::RawBsonRef::String(s) = v { s.chars().count() + 2 } else { 10 }).max().unwrap_or(0);
            ((longest.max(c.name.chars().count()) as f32) * cw + 28.0).clamp(80.0, 360.0)
        })
        .collect();
    let total = NUM_W + widths.iter().sum::<f32>();
    let mut act = None;
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(1).show(ui, |ui| {
        let view_w = ui.available_width();
        let (head, _) = ui.allocate_exact_size(egui::vec2(view_w, TABLE_HEAD_H), Sense::hover());
        let area = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::both().id_salt("sql-grid").auto_shrink(false).show_rows(ui, ROW_H, set.rows.len(), |ui, range| {
                let rows = Rows::Raw(&set.rows);
                for i in range {
                    let (rr, resp) = ui.allocate_exact_size(egui::vec2(total.max(view_w - 2.0), ROW_H), Sense::click());
                    let fill = table::row_fill(false, false, ui.rect_contains_pointer(rr) && !ui.ctx().any_popup_open(), resp.is_pointer_button_down_on(), i % 2 == 1);
                    if fill != Color32::TRANSPARENT {
                        ui.painter().rect_filled(rr, 0, fill);
                    }
                    ui.painter().text(egui::pos2(rr.left() + 18.0, rr.center().y), egui::Align2::LEFT_CENTER, (i + 1).to_string(), FontId::monospace(TXT_CAPTION), FAINT());
                    let mut x = rr.left() + NUM_W;
                    for (c, w) in set.cols.iter().zip(&widths) {
                        let cell = egui::Rect::from_min_size(egui::pos2(x, rr.top()), egui::vec2(*w, ROW_H));
                        x += w;
                        if !ui.is_rect_visible(cell) {
                            continue;
                        }
                        let (mut job, color) = match rows.value(i, &c.name) {
                            Some(v) => (sql_cell_job(v, c.kind), TEXT()),
                            None => (egui::text::LayoutJob::single_section("—".into(), mono_fmt(TXT_SMALL, FAINT(), 400.0)), FAINT()),
                        };
                        job.wrap = egui::text::TextWrapping::truncate_at_width(w - 14.0);
                        let g = ui.fonts_mut(|f| f.layout_job(job));
                        let gx = if is_numeric(c.kind) { cell.right() - 8.0 - g.size().x } else { cell.left() + 8.0 }.max(cell.left() + 8.0);
                        ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0))).galley(egui::pos2(gx, cell.center().y - g.size().y / 2.0), g, color);
                    }
                    resp.context_menu(|ui| {
                        menu_look(ui);
                        ui.label(RichText::new(format!("Row {}", i + 1)).size(TXT_CAPTION).color(FAINT()));
                        if let Some(DocsAct::CopyAs(f)) = ui.menu_button("Copy as", |ui| copy_as_menu(ui, true)).inner.flatten() {
                            act = Some((f, i));
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
        p.text(egui::pos2(head.left() + 18.0 - off, head.center().y), egui::Align2::LEFT_CENTER, "#", FontId::monospace(TXT_CAPTION), FAINT());
        let mut x = head.left() + NUM_W - off;
        for (c, w) in set.cols.iter().zip(&widths) {
            let g = p.layout_no_wrap(c.name.clone(), FontId::proportional(TXT_SMALL), MUTED());
            p.galley(egui::pos2(x + 8.0, head.center().y - g.size().y / 2.0 + optical_dy(&g)), g, MUTED());
            x += w;
        }
        p.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, LINE()));
    });
    act
}

impl App {
    /// The name a tab shows: its table, or "Query N" for a Query tab that belongs to no table.
    pub(super) fn tab_name(&self, i: usize) -> String {
        let Some(t) = self.tabs.get(i) else { return String::new() };
        if t.sql.is_some() && t.sel.coll.is_empty() {
            return format!("Query {}", self.tabs[..=i].iter().filter(|x| x.sql.is_some() && x.sel.coll.is_empty()).count());
        }
        t.sel.coll.clone()
    }

    /// Opens a Query tab for a database. It has no table, so its collection name stays empty.
    pub(super) fn sql_new_query(&mut self, conn: u64, db: String) {
        let engine = self.conn(conn).map(|c| c.meta.engine).unwrap_or_default();
        let mut t = Tab::new(Sel { conn, db, coll: String::new() }).for_engine(engine);
        t.view = View::Sql;
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
        self.reset_doc_state();
    }

    /// Runs the statement under the caret, or the whole script. `explain` puts EXPLAIN in front of the statement.
    pub(super) fn sql_run(&mut self, all: bool, explain: bool) {
        let Some(t) = self.t() else { return };
        let (conn, schema) = (t.sel.conn, run_db(t));
        let Some(s) = t.sql.as_ref().filter(|s| s.run_req == 0) else { return };
        let text = if all { s.query.clone() } else { statement_pos(&s.query, s.caret).map(|(r, _, _)| s.query[r].to_string()).unwrap_or_default() };
        if text.trim().is_empty() {
            return self.err("Nothing to run.");
        }
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(db) = self.conn(conn).and_then(|c| c.sql.clone()) else { return self.err("Connect first.") };
        let read_only = !self.conn(conn).is_some_and(|c| c.writable);
        let key = format!("{conn}/{schema}/#query");
        push_history(&mut self.prefs.history.entry(key).or_default().selector, &text);
        self.save_prefs();
        let text = if explain { format!("EXPLAIN {text}") } else { text };
        self.req += 1;
        let req = self.req;
        let Some(s) = self.tabs[self.cur].sql.as_mut() else { return };
        // Fresh handles per run, so a Stop pressed for an old run never reaches this one.
        (s.run_req, s.ran_ro, s.cancel, s.run_conn) = (req, read_only, Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)));
        self.worker.sql_run(req, db, schema, text, read_only, s.run_conn.clone(), s.cancel.clone());
    }

    /// Stops the running script: the statement in flight is interrupted and the rest never starts.
    fn sql_stop(&mut self) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref().filter(|s| s.run_req != 0) else { return };
        if let Some(db) = self.conn(t.sel.conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_kill(db, s.run_conn.load(Ordering::SeqCst), s.cancel.clone());
        }
    }

    /// Applies a finished run to its tab; a run the tab no longer waits for is dropped.
    pub(super) fn sql_ran(&mut self, req: u64, sets: Vec<ResultSet>) {
        if let Some(s) = self.tabs.iter_mut().find_map(|t| t.sql.as_mut().filter(|s| s.run_req == req)) {
            (s.run_req, s.set_sel, s.text_cache, s.sets) = (0, 0, None, sets);
        }
    }

    pub(super) fn sql_query_view(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        let (conn, free, schema) = (t.sel.conn, t.sel.coll.is_empty(), run_db(t));
        let running = t.sql.as_ref().is_some_and(|s| s.run_req != 0);
        let title = if free { self.tab_name(self.cur) } else { "Query".into() };
        let dbs: Vec<String> = self.conn(conn).map(|c| c.dbs.iter().map(|d| d.0.clone()).collect()).unwrap_or_default();
        let hist = self.prefs.history.get(&format!("{conn}/{schema}/#query")).map(|h| h.selector.clone()).unwrap_or_default();
        let (mut pick, mut load, mut picked) = (schema.clone(), None, false);
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new(&title).size(TXT_HEADING).color(BRIGHT())));
            ui.add_space(6.0);
            ui.scope(|ui| {
                quiet_look(ui);
                egui::ComboBox::from_id_salt("sql-db").selected_text(RichText::new(&schema).font(FontId::monospace(TXT_SMALL))).show_ui(ui, |ui| {
                    menu_look(ui);
                    for d in &dbs {
                        picked |= ui.selectable_value(&mut pick, d.clone(), d).clicked();
                    }
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                icon_menu(ui, paint_clock, true, |ui| {
                    ui.set_min_width(360.0);
                    ui.set_max_width(520.0);
                    if hist.is_empty() {
                        ui.label(RichText::new("Statements you run appear here.").color(MUTED()));
                    }
                    egui::ScrollArea::vertical().id_salt("sql-hist").max_height(300.0).show(ui, |ui| {
                        for h in &hist {
                            let one_line: String = h.split_whitespace().collect::<Vec<_>>().join(" ");
                            let mut job = sql_job(&one_line.chars().take(90).collect::<String>());
                            job.wrap.max_rows = 1;
                            if ui.add(quiet_button(job)).on_hover_text(h.as_str()).clicked() {
                                load = Some(h.clone());
                                ui.close();
                            }
                        }
                    });
                })
                .on_hover_text("History");
            });
        });
        ui.add_space(4.0);
        let (mut run, mut run_all, mut stop, mut explain) = (false, false, false, false);
        let status = self.t().and_then(|t| t.sql.as_ref()).map(|s| {
            let (l, c) = line_col(&s.query, s.caret);
            let n = statement_pos(&s.query, s.caret).map_or(String::new(), |(_, i, n)| format!("  ·  statement {i} of {n}"));
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(s) = &status {
                    ui.label(RichText::new(s).font(FontId::monospace(TXT_CAPTION)).color(FAINT()));
                }
            });
        });
        ui.add_space(8.0);
        if picked {
            if let Some(t) = self.tm() {
                match (free, t.sql.as_mut()) {
                    (true, _) => t.sel.db = pick,
                    (false, Some(s)) => s.run_db = pick,
                    _ => {}
                }
            }
        }
        if let (Some(text), Some(s)) = (load, self.tm().and_then(|t| t.sql.as_mut())) {
            s.query = text;
        }
        self.sql_editor(ui);
        ui.add_space(10.0);
        self.sql_result_pane(ui, running);
        if stop {
            self.sql_stop();
        }
        if run || run_all || explain {
            self.sql_run(run_all, explain);
        }
    }

    /// The script editor with line numbers, token colors and a faint band behind the statement under the caret.
    fn sql_editor(&mut self, ui: &mut egui::Ui) {
        let id = egui::Id::unique(("sql-editor", self.cur));
        let focused = ui.memory(|m| m.has_focus(id));
        let avail = ui.available_height();
        let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) else { return };
        let lines = s.query.split('\n').count();
        let h = (lines as f32 * LINE_H + 24.0).clamp(96.0, (avail * 0.5).max(96.0));
        let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let mut job = sql_job(buf.as_str());
            for sec in &mut job.sections {
                sec.format.line_height = Some(LINE_H);
            }
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("sql-ed").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                // Reserved first so the band is drawn under the text.
                let band = ui.painter().add(egui::Shape::Noop);
                let out = egui::TextEdit::multiline(&mut s.query)
                    .id(id)
                    .font(FontId::monospace(TXT_SMALL))
                    .frame(egui::Frame::new().inner_margin(egui::Margin { left: GUTTER_W as i8, right: 12, top: 10, bottom: 10 }))
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, 96.0))
                    .lock_focus(true)
                    .hint_text(RichText::new("SELECT * FROM orders;").font(FontId::monospace(TXT_SMALL)).color(FAINT()))
                    .layouter(&mut layout)
                    .show(ui);
                if let Some(r) = out.cursor_range {
                    s.caret = usize::from(r.primary.index);
                }
                let (g, gp, rect) = (&out.galley, out.galley_pos, out.response.rect);
                if let Some((r, _, _)) = statement_pos(&s.query, s.caret).filter(|p| p.2 > 1) {
                    let at = |b: usize| s.query[..b].chars().count();
                    let (a, z) = (g.pos_from_cursor(CCursor::new(at(r.start))), g.pos_from_cursor(CCursor::new(at(r.end))));
                    let band_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), gp.y + a.top()), egui::pos2(rect.right(), gp.y + z.bottom()));
                    ui.painter().set(band, egui::Shape::rect_filled(band_rect, 0.0, tint(PRIMARY(), 22)));
                }
                let cur_line = line_col(&s.query, s.caret).0;
                let (mut n, mut fresh) = (0, true);
                for row in &g.rows {
                    if fresh {
                        n += 1;
                        let y = gp.y + row.pos.y + row.height() / 2.0;
                        ui.painter().text(egui::pos2(rect.left() + GUTTER_W - 14.0, y), egui::Align2::RIGHT_CENTER, n.to_string(), FontId::monospace(TXT_CAPTION), if n == cur_line { MUTED() } else { FAINT() });
                    }
                    fresh = row.ends_with_newline;
                }
            });
        });
    }

    /// The result tabs, the Table or Text switch and the selected statement's outcome.
    fn sql_result_pane(&mut self, ui: &mut egui::Ui, running: bool) {
        let cur = self.cur;
        let tab_coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
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
        let (mut pick, mut text_mode, mut copy) = (None, s.text_mode, None);
        let sel = s.set_sel.min(s.sets.len() - 1);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let w = (ui.available_width() - 190.0).max(120.0);
            ui.allocate_ui(egui::vec2(w, BUTTON_H + 8.0), |ui| {
                egui::ScrollArea::horizontal().id_salt("sql-sets").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (i, set) in s.sets.iter().enumerate() {
                            if set_tab(ui, i, set, i == sel, s.ran_ro).clicked() {
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
                });
                segmented(ui, &mut text_mode, &[(false, "Table"), (true, "Text")]);
            });
        });
        ui.add_space(6.0);
        s.text_mode = text_mode;
        if let Some(i) = pick {
            (s.set_sel, s.text_cache) = (i, None);
        }
        let (error, stmt, affected, ms, capped) = {
            let x = &s.sets[sel];
            (x.error.clone(), x.stmt.clone(), x.affected, x.ms, x.capped)
        };
        let mut cols_for_copy = Vec::new();
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
                ui.label(RichText::new(format!("Capped at {} rows. The rest was not read.", db::sql::RUN_ROWS_MAX)).font(FontId::monospace(TXT_SMALL)).color(WARNING()));
                ui.add_space(4.0);
            }
            cols_for_copy = s.sets[sel].cols.clone();
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
            } else if let Some(c) = grid(ui, &s.sets[sel]) {
                copy = Some((c.0, Some(c.1)));
            }
        }
        if let Some((fmt, row)) = copy {
            let rows: Vec<Document> = s.sets[sel].rows.iter().enumerate().filter(|(i, _)| row.is_none_or(|r| r == *i)).map(|(_, r)| r.owned()).collect();
            let cols: Vec<CopyCol> = cols_for_copy.iter().map(|c| CopyCol { name: c.name.clone(), numeric: is_numeric(c.kind) }).collect();
            let table = if tab_coll.is_empty() { "result".to_string() } else { tab_coll };
            let ctx = ui.ctx().clone();
            self.copy_sheet(&ctx, fmt, &rows, &cols, &table, &[], self.sql_bs());
        }
    }

    /// QUARRY_TEST_SQL=query: a Query tab with the multi-statement script of the design and its three outcomes.
    pub(super) fn sql_query_demo(&mut self) {
        let mut t = Tab::new(Sel { conn: 3, db: "shop".into(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
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
        let query = "-- Paid orders over the last week, by customer\nSELECT c.name, c.email, COUNT(*) AS orders, SUM(o.total) AS spent\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'paid'\n  AND o.created_at >= CURDATE() - INTERVAL 7 DAY\nGROUP BY c.id\nORDER BY spent DESC;\n\nUPDATE orders SET status = 'shipped' WHERE id IN (4405, 4407);\nSELECT * FROM shipments WHERE order_id = 4405;";
        let first = ResultSet {
            stmt: query.lines().skip(1).take(7).collect::<Vec<_>>().join("\n"),
            cols: vec![col("name", "VARCHAR"), col("email", "VARCHAR"), col("orders", "BIGINT"), col("spent", "DECIMAL")],
            rows: people.iter().filter_map(|&(n, e, o, s)| db::Row::from_doc(&doc! { "name": n, "email": e, "orders": o, "spent": s }).ok()).collect(),
            affected: None,
            error: None,
            ms: 31,
            capped: false,
        };
        let refused = ResultSet { stmt: "UPDATE orders SET status = 'shipped' WHERE id IN (4405, 4407)".into(), cols: Vec::new(), rows: Vec::new(), affected: None, error: Some("This statement is not allowed on a read-only connection. Unlock writes to run it.".into()), ms: 0, capped: false };
        let last = ResultSet {
            stmt: "SELECT * FROM shipments WHERE order_id = 4405".into(),
            cols: vec![col("id", "INT"), col("order_id", "INT"), col("carrier", "VARCHAR")],
            rows: db::Row::from_doc(&doc! { "id": 912_i64, "order_id": 4405_i64, "carrier": "DHL" }).into_iter().collect(),
            affected: None,
            error: None,
            ms: 4,
            capped: false,
        };
        if let Some(s) = t.sql.as_mut() {
            s.caret = query.lines().take(6).map(|l| l.chars().count() + 1).sum::<usize>() - 1;
            (s.query, s.sets, s.ran_ro) = (query.into(), vec![first, refused, last], true);
        }
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let t = "SELECT 1;\n\nUPDATE a SET b = 2;\nSELECT 'x;y';";
        let (r, i, n) = statement_pos(t, 3).unwrap();
        assert_eq!((&t[r], i, n), ("SELECT 1", 1, 3));
        let (_, i, _) = statement_pos(t, 14).unwrap();
        assert_eq!(i, 2);
        // A caret past the end stays with the last statement.
        assert_eq!(statement_pos(t, 999).unwrap().1, 3);
        assert!(statement_pos("  ", 0).is_none());
    }

    #[test]
    fn kind_skips_comments() {
        assert_eq!(stmt_kind("-- hi\n  select 1"), "SELECT");
        assert_eq!(stmt_kind(""), "");
    }
}
