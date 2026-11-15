//! The title bar's path: connection, database and table as crumbs, the connection's tag and the write lock chip.

use super::*;

/// The faint second line of a tab. The connection comes first so a narrow tab cuts the database, and two tabs on the
/// same table of different connections never read alike.
pub(super) fn tab_second_line(conn: &str, scope: &str) -> String {
    match (conn.is_empty(), scope.is_empty()) {
        (false, false) => format!("{conn} \u{b7} {scope}"),
        (false, true) => conn.to_string(),
        _ => scope.to_string(),
    }
}

const CRUMB_MENU_W: f32 = 260.0;

/// A path segment that opens a menu. Returns the response to anchor the popup on.
fn crumb(ui: &mut egui::Ui, text: &str, color: Color32, label: &str) -> egui::Response {
    let mut job = LayoutJob::single_section(text.to_string(), mono_fmt(TXT_SMALL, color, W_SEMIBOLD));
    job.wrap = egui::text::TextWrapping::truncate_at_width(220.0);
    let g = ui.painter().layout_job(job);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 30.0, 24.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, R_SM, hover_fill(state_of(&r)));
    p.galley(egui::pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, color);
    chevron(p, egui::pos2(rect.right() - 11.0, rect.center().y), true, if r.hovered() { MUTED() } else { FAINT() });
    focus_ring(ui, &r, rect, R_SM as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The menu under a crumb: a filter, the recent names, then all of them. Returns the name picked. The names are read only
/// while the menu is open, since the crumbs are drawn every frame.
fn crumb_menu(resp: &egui::Response, kind: &str, current: &str, recent: &[String], all: impl FnOnce() -> Vec<String>) -> Option<String> {
    let id = resp.id.with("filter");
    if resp.clicked() {
        resp.ctx.data_mut(|d| d.insert_temp(id, String::new()));
    }
    let plural = format!("{}s", kind.to_lowercase());
    let mut picked = None;
    egui::Popup::menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(CRUMB_MENU_W).show(|ui| {
        menu_look(ui);
        let all = all();
        let all = all.as_slice();
        ui.set_min_width(CRUMB_MENU_W);
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut q = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_default();
        let hint = RichText::new(format!("Filter {plural}")).color(FAINT());
        let r = ui.add(egui::TextEdit::singleline(&mut q).font(FontId::proportional(TXT_BODY)).hint_text(hint).desired_width(f32::INFINITY));
        r.request_focus();
        let tok = tokens(&q);
        let hit = |n: &String| tok.iter().all(|w| n.to_lowercase().contains(w.as_str()));
        let recent: Vec<&String> = recent.iter().filter(|n| all.contains(n) && hit(n)).collect();
        let rest: Vec<&String> = all.iter().filter(|n| hit(n) && !recent.contains(n)).collect();
        // Keeps the menu inside the window however many names there are.
        let room = (ui.ctx().content_rect().height() - 160.0).clamp(120.0, 360.0);
        egui::ScrollArea::vertical().max_height(room).show(ui, |ui| {
            if !recent.is_empty() {
                caption_row(ui, "RECENT");
                for n in &recent {
                    picked = picked.take().or_else(|| crumb_row(ui, kind, n, current));
                }
                if !rest.is_empty() {
                    caption_row(ui, "ALL");
                }
            }
            for n in &rest {
                picked = picked.take().or_else(|| crumb_row(ui, kind, n, current));
            }
            if recent.is_empty() && rest.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(if all.is_empty() { format!("No {plural} loaded") } else { "Nothing matches".to_string() }).color(FAINT()));
            }
        });
        ui.data_mut(|d| d.insert_temp(id, q));
        if picked.is_some() {
            ui.close();
        }
    });
    picked
}

fn caption_row(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(CRUMB_MENU_W, 22.0), Sense::hover());
    let g = ui.painter().layout_job(LayoutJob::single_section(text.into(), ui_fmt(TXT_CAPTION, FAINT(), W_SEMIBOLD, 0.5)));
    ui.painter().galley(egui::pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, FAINT());
}

fn crumb_row(ui: &mut egui::Ui, kind: &str, name: &str, current: &str) -> Option<String> {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(CRUMB_MENU_W, 28.0), Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MenuItem, true, format!("{kind} {name}")));
    let on = name == current;
    let state = state_of(&r);
    let p = ui.painter();
    p.rect_filled(rect, R_SM, if state == 2 { menu_pressed() } else if state == 1 { MENU_HOVER() } else { Color32::TRANSPARENT });
    let color = if on { BRIGHT() } else { TEXT() };
    let g = truncated(p, name, FontId::monospace(TXT_SMALL), color, CRUMB_MENU_W - 36.0);
    p.galley(egui::pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, color);
    if on {
        p.circle_filled(egui::pos2(rect.right() - 14.0, rect.center().y), 3.0, PRIMARY());
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked().then(|| name.to_string())
}

fn mono_label(text: &str, color: Color32, weight: f32) -> LayoutJob {
    LayoutJob::single_section(text.to_string(), mono_fmt(TXT_SMALL, color, weight))
}

impl App {
    /// The title bar path for the current tab and the chips after it.
    pub(super) fn crumbs(&mut self, ui: &mut egui::Ui, s: &Sel) {
        let Some((name, writable, tag)) = self.conn(s.conn).map(|c| (c.meta.name.clone(), c.writable, c.meta.tag.clone())) else { return };
        ui.label(mono_label(&name, MUTED(), 400.0));
        ui.label(mono_label("/", FAINT(), 400.0));
        // The Query view runs its statements where its run target says, which a table's tab can point elsewhere, so
        // there the crumbs name that database and schema.
        let query = self.t().filter(|t| t.view == View::Sql && t.sql.is_some()).map(|t| (super::sql::query::run_db(t), super::sql::query::run_schema_of(t)));
        let pg = self.is_pg(s.conn);
        let run = query.as_ref().map_or(s.db.clone(), |q| q.0.clone());
        // A PostgreSQL path has a schema level between the database and the table.
        let place = if query.is_some() || s.schema.is_empty() || s.coll.is_empty() { run.clone() } else { format!("{} / {}", s.db, s.schema) };
        let r = crumb(ui, &place, if s.coll.is_empty() { BRIGHT() } else { TEXT() }, "Database crumb");
        let dbs = || self.conn(s.conn).map(|c| c.dbs.iter().map(|d| d.0.clone()).collect::<Vec<String>>()).unwrap_or_default();
        if let Some(db) = crumb_menu(&r, "Database", &run, self.prefs.recent_dbs(s.conn), dbs) {
            self.pick_crumb_db(s.conn, db);
        }
        // The Query view has no schema field, so a PostgreSQL one picks the schema its statements run in here.
        if let (true, Some((run, typed))) = (pg, query) {
            let now = if typed.is_empty() { self.sel_schema(&Sel { db: run.clone(), schema: String::new(), ..s.clone() }) } else { typed };
            let unlisted = self.conn(s.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == run)).is_some_and(|d| d.1.is_none());
            if unlisted && !self.sql_asked.contains(&(s.conn, run.clone())) {
                self.pg_ask_schemas(s.conn, &run);
            }
            ui.label(mono_label("/", FAINT(), 400.0));
            let r = crumb(ui, &now, TEXT(), "Schema crumb");
            let listed = || self.conn(s.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == run)).and_then(|d| d.1.clone()).unwrap_or_default();
            if let Some(schema) = crumb_menu(&r, "Schema", &now, &[], listed) {
                self.pick_crumb_schema(s.conn, &run, schema);
            }
        }
        let tab = self.tab_name(self.cur);
        if s.coll.is_empty() {
            // A query tab has no table, so its own name ends the path. One label with the slash, so no widget
            // carries the bare tab name that tests and the tab strip look up.
            let mut job = mono_label("/ ", FAINT(), 400.0);
            job.append(&tab, 0.0, mono_fmt(TXT_SMALL, BRIGHT(), W_SEMIBOLD));
            ui.label(job);
        } else {
            ui.label(mono_label("/", FAINT(), 400.0));
            let r = crumb(ui, &tab, BRIGHT(), "Table crumb");
            if let Some(t) = crumb_menu(&r, "Table", &s.coll, &[], || self.tables_of(&Sel { coll: String::new(), ..s.clone() })) {
                self.open_collection(Sel { coll: t, ..s.clone() }, Open::Here);
            }
        }
        ui.add_space(6.0);
        if !tag.is_empty() {
            let g = tag_text(ui.painter(), &tag, 140.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(g.size().x + 14.0, 18.0), Sense::hover());
            paint_chip(ui.painter(), rect, g, tag_color(&tag), true, 0);
            ui.add_space(6.0);
        }
        if self.prefs.always_writable {
            // No lock to toggle: writes are always on.
        } else if writable {
            if lock_chip(ui, true).on_hover_text("Click to lock this connection again").clicked() {
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == s.conn) {
                    c.writable = false;
                }
                self.ok("Writes locked");
            }
        } else if lock_chip(ui, false).on_hover_text("Click to allow edits on this connection for this session").clicked() {
            self.modal = Some(Modal::Unlock(s.conn, None));
        }
    }

    /// A database picked in the crumb menu. A Query tab moves to it and a table's Query view runs its statements there;
    /// any other view reveals it in the explorer.
    fn pick_crumb_db(&mut self, conn: u64, db: String) {
        // A jump opens no tab, so opening one is not what records the database as recent.
        if self.prefs.note_recent_db(conn, &db) {
            self.save_prefs();
        }
        let Some(free) = self.t().filter(|t| t.view == View::Sql && (t.sel.coll.is_empty() || t.sql.is_some())).map(|t| t.sel.coll.is_empty()) else {
            return self.jump(Jump::Db(conn, db), Open::Here);
        };
        // Another PostgreSQL database has its own schemas, so the tab moves to the one its sidebar shows.
        let schema = self.ns_of(conn, &db).to_string();
        if self.is_pg(conn) && !self.sql_asked.contains(&(conn, db.clone())) {
            self.pg_ask_schemas(conn, &db);
        }
        let Some(t) = self.tm() else { return };
        let own = db == t.sel.db;
        if free {
            (t.sel.db, t.sel.schema) = (db, schema);
            // The tab's own database and schema are now the whole target, so a run target left by a script goes.
            if let Some(q) = t.sql.as_mut() {
                (q.run_db, q.run_schema) = (String::new(), String::new());
            }
        } else if let Some(q) = t.sql.as_mut() {
            // A table's Query view keeps its table and only runs elsewhere. Its own database runs in the table's schema.
            q.run_schema = if own { String::new() } else { schema };
            q.run_db = db;
        }
    }

    /// A schema picked in the crumb of a PostgreSQL Query view. Its tables load for the editor's suggestions.
    fn pick_crumb_schema(&mut self, conn: u64, run: &str, schema: String) {
        self.pg_load_tables(conn, run, &schema);
        let Some(t) = self.tm() else { return };
        let free = t.sel.coll.is_empty();
        let Some(q) = t.sql.as_mut() else { return };
        // A run target in another database is where the statements run, so the tab's own schema would not reach them.
        if free && (q.run_db.is_empty() || q.run_db == t.sel.db) {
            // A schema left from a script or a saved tab would win over the pick.
            q.run_schema.clear();
            t.sel.schema = schema;
        } else {
            q.run_schema = schema;
        }
    }
}

/// "Read-only" with an Unlock link, or a filled red "Writes unlocked" with a Lock link, as one click target. The
/// accessible names stay READ-ONLY and WRITES ON, which the tests and screen readers already know.
fn lock_chip(ui: &mut egui::Ui, writes: bool) -> egui::Response {
    let (text, link, fg) = if writes { ("Writes unlocked", "Lock", ALERT()) } else { ("Read-only", "Unlock", WARNING()) };
    let ink = if writes { Color32::WHITE } else { fg };
    let g = ui.painter().layout_job(LayoutJob::single_section(text.into(), ui_fmt(TXT_SMALL, ink, W_MEDIUM, 0.0)));
    let l = ui.painter().layout_job(LayoutJob::single_section(link.into(), ui_fmt(TXT_SMALL, if writes { Color32::WHITE } else { PRIMARY() }, W_MEDIUM, 0.0)));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0 + g.size().x + 10.0 + l.size().x + 10.0, 22.0), Sense::click());
    let state = state_of(&r);
    let p = ui.painter();
    let (fill, line) = if writes { (fg.lerp_to_gamma(Color32::WHITE, [0.0, 0.1, 0.0][state]), fg) } else { (tint(fg, [31, 51, 71][state]), tint(fg, 90)) };
    p.rect(rect, rect.height() / 2.0, fill, Stroke::new(1.0, line), egui::StrokeKind::Inside);
    glyphs::padlock(p, egui::pos2(rect.left() + 15.0, rect.center().y), !writes, ink);
    let x = rect.left() + 28.0;
    let lx = x + g.size().x + 10.0;
    p.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, ink);
    p.galley(egui::pos2(lx, rect.center().y - l.size().y / 2.0 + optical_dy(&l)), l, ink);
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, if writes { "WRITES ON" } else { "READ-ONLY" }));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}
