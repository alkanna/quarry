//! Other saved connections attached to a Shell tab: the saved entry, the chips in the toolbar, the
//! Attach dialog, and what the runner and the language server are told about each one.

use super::rail::{engine_mark, mark_color};
use super::*;

/// A connection attached to a shell tab, saved with it. `name` is the global the script uses.
#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug)]
#[serde(default)]
pub(super) struct Attached {
    pub(super) conn: u64,
    pub(super) db: String,
    pub(super) name: String,
    /// Writes also need the connection itself to be unlocked, so this alone never allows one.
    pub(super) allow_writes: bool,
    /// PostgreSQL only: the schema the script runs in (its search_path). `db` is then the database.
    pub(super) schema: String,
    /// PostgreSQL only: the tables of `schema`, listed when attaching and before a run. Not saved, since they go stale.
    #[serde(skip)]
    pub(super) tables: Vec<String>,
}

/// The schema a PostgreSQL script starts in when none was picked, as a new database has it.
pub(super) const PG_SCHEMA: &str = "public";

/// The state of the Attach dialog.
#[derive(Default)]
pub(super) struct AttachForm {
    filter: String,
    conn: Option<u64>,
    db: String,
    /// PostgreSQL only: the schema to attach, and the schemas of `db` once they are listed.
    schema: String,
    schemas: Vec<String>,
    /// The (connection, database) whose schemas were asked for, so each is asked once.
    asked: Option<(u64, String)>,
    name: String,
    /// The name was typed, so a new database no longer renames it.
    name_set: bool,
    writes: bool,
    focused: bool,
}

/// What the language server was told about an attachment, kept to tell when it changed. Holding the
/// Arcs keeps each pointer unique, so pointer equality means unchanged.
pub(super) enum AttTyped {
    Mongo(Vec<(String, Arc<Fields>)>),
    Sql(Option<Arc<db::sql::schema::Schema>>),
}

impl AttTyped {
    pub(super) fn same(&self, o: &AttTyped) -> bool {
        match (self, o) {
            (AttTyped::Mongo(a), AttTyped::Mongo(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0 && Arc::ptr_eq(&x.1, &y.1)),
            (AttTyped::Sql(Some(a)), AttTyped::Sql(Some(b))) => Arc::ptr_eq(a, b),
            (AttTyped::Sql(None), AttTyped::Sql(None)) => true,
            _ => false,
        }
    }
}

/// What the language server needs of one attachment: its name, its collection or table names, and the types behind them.
pub(super) struct AttachedInfo {
    pub(super) name: String,
    pub(super) names: Vec<String>,
    pub(super) typed: AttTyped,
}

/// The database name as a JS identifier: other characters become `_`, a leading digit gets one in front,
/// and a reserved word gets one after it.
pub(super) fn default_name(db: &str) -> String {
    let mut s: String = db.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '$' { c } else { '_' }).collect();
    if s.is_empty() || s.chars().all(|c| c == '_') {
        return "conn".into();
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if JS_RESERVED.contains(&s.as_str()) {
        s.push('_');
    }
    s
}

/// Why `name` cannot be an attachment's global, or None when it can. `taken` lists the names already in
/// use with what uses them, which the message names.
pub(super) fn name_problem(name: &str, taken: &[(String, String)]) -> Option<String> {
    if name.is_empty() {
        return Some("Enter a name.".into());
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$') || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return Some("Use letters, digits, _ or $, and do not start with a digit.".into());
    }
    if JS_RESERVED.contains(&name) {
        return Some(format!("{name} is a reserved word in JavaScript."));
    }
    taken.iter().find(|t| t.0 == name).map(|t| format!("{name} is already {}.", t.1))
}

/// "8.0.36" as "8.0".
fn short_version(v: &str) -> String {
    v.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// The mark and color of a connection's engine, as the rail paints them.
pub(super) fn mark_of(c: &Conn) -> (&'static str, &'static str, Color32) {
    let (mark, product) = engine_mark(c.meta.engine, &c.server.product);
    (mark, product, mark_color(mark))
}

/// AttTyped text for a pill's width and paint, in the caption or body size.
fn pill_galley(ui: &egui::Ui, text: &str, fmt: egui::TextFormat) -> Arc<egui::Galley> {
    ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), fmt))
}

/// How a pill in the Connections row reads.
pub(super) struct Pill<'a> {
    pub(super) mark: &'a str,
    pub(super) color: Color32,
    pub(super) label: String,
    pub(super) note: Option<(&'a str, Color32)>,
    /// The tab's own connection, tinted with the accent; attachments are neutral.
    pub(super) own: bool,
    pub(super) hint: String,
}

/// One connection in the toolbar: its engine mark, "name · db", a note such as read-only and, for an
/// attachment, a painted x. Returns true when the x was clicked.
/// `max_w` is the pill's share of the toolbar: a longer "name · db" is cut with an ellipsis, so the pills
/// never run under the buttons on the right. The hover text keeps the whole name.
pub(super) fn conn_pill(ui: &mut egui::Ui, id: egui::Id, p: &Pill, close: bool, max_w: f32) -> bool {
    let note = p.note.map(|(t, c)| pill_galley(ui, t, mono_fmt(TXT_CAPTION, c, 400.0)));
    let (pad, gap, mark_d, x_d) = (6.0, 6.0, 18.0, 16.0);
    let fixed = pad + mark_d + gap + note.as_ref().map_or(0.0, |n| gap + n.size().x) + if close { gap + x_d } else { 0.0 } + pad;
    let mut job = egui::text::LayoutJob::single_section(p.label.clone(), egui::TextFormat::simple(FontId::proportional(TXT_BODY), BRIGHT()));
    job.wrap = egui::text::TextWrapping::truncate_at_width((max_w - fixed).max(40.0));
    let label = ui.painter().layout_job(job);
    let w = pad + mark_d + gap + label.size().x + note.as_ref().map_or(0.0, |n| gap + n.size().x) + if close { gap + x_d } else { 0.0 } + pad;
    let (rect, body) = ui.allocate_exact_size(egui::vec2(w, CHIP_H + 4.0), Sense::hover());
    let x_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - pad - x_d / 2.0, rect.center().y), egui::vec2(x_d, x_d));
    let x = close.then(|| ui.interact(x_rect, id.with("x"), Sense::click()));
    let x_state = x.as_ref().map_or(0, state_of);
    let state = if body.hovered() || x_state > 0 { 1 } else { 0 };
    let (fill, line) = if p.own { (tint(PRIMARY(), [16, 24][state]), tint(PRIMARY(), 60)) } else { ([PANEL(), HOVER()][state], BORDER()) };
    let pt = ui.painter();
    pt.rect(rect, rect.height() / 2.0, fill, Stroke::new(1.0, line), egui::StrokeKind::Inside);
    let mark_c = egui::pos2(rect.left() + pad + mark_d / 2.0, rect.center().y);
    pt.circle_filled(mark_c, mark_d / 2.0, tint(p.color, 55));
    pt.text(mark_c, egui::Align2::CENTER_CENTER, p.mark, FontId::monospace(8.0), p.color);
    let mut cx = rect.left() + pad + mark_d + gap;
    pt.galley(egui::pos2(cx, rect.center().y - label.size().y / 2.0 + optical_dy(&label)), label.clone(), BRIGHT());
    cx += label.size().x + gap;
    if let Some(n) = note {
        pt.galley(egui::pos2(cx, rect.center().y - n.size().y / 2.0 + optical_dy(&n)), n, PRIMARY());
    }
    let mut removed = false;
    if let Some(x) = x {
        pt.circle_filled(x_rect.center(), x_d / 2.0, hover_fill(x_state));
        let (c, s) = (x_rect.center(), Stroke::new(1.3, if x_state > 0 { BRIGHT() } else { MUTED() }));
        pt.line_segment([c + egui::vec2(-3.0, -3.0), c + egui::vec2(3.0, 3.0)], s);
        pt.line_segment([c + egui::vec2(-3.0, 3.0), c + egui::vec2(3.0, -3.0)], s);
        focus_ring(ui, &x, x_rect, x_d / 2.0);
        x.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Detach {}", p.label)));
        removed = x.on_hover_text("Detach").on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
    }
    if label.elided {
        body.on_hover_text(format!("{}\n{}", p.label, p.hint));
    } else {
        body.on_hover_text(&p.hint);
    }
    removed
}

/// The "+ Attach" button after the pills, painted so its plus sits centered. True when clicked.
pub(super) fn attach_button(ui: &mut egui::Ui) -> bool {
    let g = pill_galley(ui, "Attach", ui_fmt(TXT_SMALL, Color32::PLACEHOLDER, 400.0, 0.0));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 34.0, CHIP_H + 4.0), Sense::click());
    let state = state_of(&r);
    let p = ui.painter();
    p.rect(rect, rect.height() / 2.0, hover_fill(state), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let color = if state > 0 { BRIGHT() } else { MUTED() };
    let c = egui::pos2(rect.left() + 14.0, rect.center().y);
    let s = Stroke::new(1.3, color);
    p.line_segment([c - egui::vec2(3.5, 0.0), c + egui::vec2(3.5, 0.0)], s);
    p.line_segment([c - egui::vec2(0.0, 3.5), c + egui::vec2(0.0, 3.5)], s);
    let dy = optical_dy(&g);
    p.galley(egui::pos2(rect.left() + 24.0, rect.center().y - g.size().y / 2.0 + dy), g, color);
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Attach a connection"));
    r.on_hover_text("Use another saved connection in this script").on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

impl App {
    /// Names already in use as globals of the current Shell tab, each with what uses it. `engines` are the
    pub(super) fn shell_taken(&self) -> Vec<(String, String)> {
        let Some(t) = self.t() else { return Vec::new() };
        let main_sql = t.sql.is_some();
        let mut out: Vec<(String, String)> = Vec::new();
        let mut add = |n: &str, why: String| {
            if !out.iter().any(|o| o.0 == n) {
                out.push((n.to_string(), why));
            }
        };
        // Refused whatever the engines, so a later attachment of another engine cannot collide with it.
        for n in ["db", "sql", "table", "require", "console", "log", "_", "dayjs", "ObjectId", "ISODate", "randomId", "UUIDv4", "UUIDv1"] {
            add(n, "a name the shell provides".into());
        }
        for p in &self.prefs.shell_packages {
            add(&package_ident(p), format!("the name of the package {}", db::package_spec(p).0));
        }
        // The runner tells this tab's own connection apart from an attachment by its name.
        if let Some(c) = self.conn(t.sel.conn) {
            add(&c.meta.name, "the name of this tab's connection".into());
        }
        for n in self.conn(t.sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == t.sel.db)).and_then(|d| d.1.as_ref()).into_iter().flatten() {
            add(n, format!("{} of {}", if main_sql { "a table" } else { "a collection" }, t.sel.db));
        }
        for a in &t.shell.attached {
            add(&a.name, "used by another attached connection".into());
        }
        out
    }

    /// QUARRY_TEST_SQL=shellattach: the SQL shell with a read-only MongoDB connection attached and a run that used both.
    /// QUARRY_TEST_SQL=attach: the Attach dialog with that connection picked. Nothing here touches the Keychain or a server.
    pub(super) fn attach_demo(&mut self) {
        let colls = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let mongo = |id: u64, name: &str, tag: &str, status: Status, dbs: Vec<(String, Option<Vec<String>>)>, version: &str| Conn { dbs, masked_uri: "mongodb://app:****@travels.internal:27017".into(), server: ServerInfo { version: version.into(), ..Default::default() }, ..Conn::new(ConnMeta { id, name: name.into(), tag: tag.into(), ..Default::default() }, status) };
        self.conns.insert(0, mongo(5, "travels", "Production", Status::Ready, vec![("admin".into(), colls(&["system.users"])), ("app".into(), colls(&["bookings", "trips", "users"]))], "8.0.4"));
        for (id, name, tag, product, version) in [(6, "shop-staging", "Staging", "MySQL", "8.0.36"), (7, "analytics", "Internal", "MariaDB", "11.4.2")] {
            self.conns.push(Conn { server: ServerInfo { version: version.into(), product: product.into(), ..Default::default() }, ..Conn::new(ConnMeta { id, name: name.into(), tag: tag.into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Idle) });
        }
        if std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("attach") {
            self.modal = Some(Modal::Attach(Box::new(AttachForm { conn: Some(5), db: "app".into(), name: "travels".into(), name_set: true, focused: true, ..Default::default() })));
            return;
        }
        let t = &mut self.tabs[0];
        t.shell.attached = vec![Attached { conn: 5, db: "app".into(), name: "travels".into(), ..Default::default() }];
        t.shell.code = "// Paid orders this week, with the traveller tier stored in Mongo
async function run() {
  const orders = await sql`
    SELECT id, customer_id, total FROM orders
    WHERE status = 'paid' AND created_at >= NOW() - INTERVAL 7 DAY`

  const ids = orders.map((o) => o.customer_id)
  const users = await travels.users
    .find({ legacyId: { $in: ids } })
    .toArray()
  const tier = new Map(users.map((u) => [u.legacyId, u.tier]))

  return orders.map((o) => ({ ...o, tier: tier.get(o.customer_id) ?? null }))
}".into();
        let rows = [(4410_i64, 1284_i64, "112.00", Some("gold")), (4407, 61, "1,204.10", Some("platinum")), (4405, 430, "54.20", None), (4403, 12, "15.25", Some("silver"))];
        t.shell.set_value(Bson::Array(rows.iter().map(|&(id, c, total, tier)| Bson::Document(doc! { "id": id, "customer_id": c, "total": total, "tier": tier.map_or(Bson::Null, Bson::from) })).collect()));
        t.shell.ms = Some(141);
        t.shell.queries = vec![
            db::ShellQuery { conn: "shop-prod".into(), text: "SELECT id, customer_id, total FROM orders WHERE status = 'paid' AND created_at >= NOW() - INTERVAL 7 DAY".into(), params: String::new(), rows: Some(38), affected: None, error: None, ms: 22 },
            db::ShellQuery { conn: "travels".into(), text: "users.find({ legacyId: { $in: [38 ids] } })".into(), params: String::new(), rows: Some(36), affected: None, error: None, ms: 119 },
        ];
    }

    /// Opens the Attach dialog for the current tab.
    pub(super) fn open_attach(&mut self) {
        self.modal = Some(Modal::Attach(Box::default()));
    }

    /// Asks for what an attached database needs on the editor side: the table list and schema of a SQL
    /// one. Collections of a Mongo connection arrive when it connects. Demo mode has no server.
    pub(super) fn attach_load(&mut self, conn: u64, db: &str) {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        if self.demo {
            return;
        }
        if sql.dialect == db::sql::Dialect::Postgres {
            // The schema is the attachment's own, so each PostgreSQL attachment of this tab on `db` asks for its tables.
            let schemas: Vec<String> = self.t().map(|t| t.shell.attached.iter().filter(|a| a.conn == conn && a.db == db).map(|a| a.schema.clone()).collect()).unwrap_or_default();
            for s in schemas {
                self.worker.pg_shell_list(conn, sql.clone(), db.to_string(), if s.is_empty() { PG_SCHEMA.into() } else { s });
            }
            return;
        }
        if self.sql_asked.insert((conn, db.to_string())) {
            self.worker.sql_tables(conn, sql.clone(), db.to_string());
        }
        if self.sql_schema_asked.insert((conn, db.to_string())) {
            self.worker.sql_schema(conn, sql, db.to_string());
        }
    }

    /// Whether any target of the current tab's script can be written to: its own connection, or an
    /// attachment that is allowed to write and whose connection is unlocked.
    pub(super) fn shell_writable(&self) -> bool {
        self.sel_writable() || self.t().is_some_and(|t| t.shell.attached.iter().any(|a| a.allow_writes && self.conn(a.conn).is_some_and(|c| c.writable)))
    }

    /// The tab's own connection as a pill; `locked` adds the read-only note.
    pub(super) fn own_pill(&self, name: &str, db: &str, locked: bool) -> Pill<'static> {
        let (mark, color) = self.t().and_then(|t| self.conn(t.sel.conn)).map_or(("MO", SUCCESS()), |c| {
            let m = mark_of(c);
            (m.0, m.2)
        });
        Pill { mark, color, label: format!("{name}  ·  {db}"), note: locked.then_some(("read-only", PRIMARY())), own: true, hint: "This tab's connection".into() }
    }

    /// The name, whether it is SQL and the mark color of every connection the script reaches, for the Queries panel.
    pub(super) fn shell_kinds(&self) -> Vec<(String, bool, Color32)> {
        let Some(t) = self.t() else { return Vec::new() };
        let own = self.conn(t.sel.conn).map(|c| (c.meta.name.clone(), c.meta.engine.is_sql(), mark_of(c).2));
        own.into_iter().chain(t.shell.attached.iter().filter_map(|a| self.conn(a.conn).map(|c| (a.name.clone(), c.meta.engine.is_sql(), mark_of(c).2)))).collect()
    }

    /// The names, engines and read-only state of the attachments for the toolbar.
    pub(super) fn attached_pills(&self) -> Vec<Pill<'static>> {
        let Some(t) = self.t() else { return Vec::new() };
        t.shell
            .attached
            .iter()
            .map(|a| {
                let c = self.conn(a.conn);
                let (mark, color) = c.map_or(("??", FAINT()), |c| {
                    let m = mark_of(c);
                    (m.0, m.2)
                });
                let ready = c.is_some_and(|c| c.status == Status::Ready);
                let writable = a.allow_writes && c.is_some_and(|c| c.writable);
                let note = if c.is_none() { Some(("deleted", ALERT())) } else if !ready { Some(("not connected", WARNING())) } else if !writable { Some(("read-only", PRIMARY())) } else { None };
                let place = if a.schema.is_empty() { a.db.clone() } else { format!("{}.{}", a.db, a.schema) };
                let what = c.map_or("a connection that no longer exists".to_string(), |c| format!("{}, database {}", c.meta.name, place));
                Pill { mark, color, label: format!("{}  ·  {}", a.name, place), note, own: false, hint: format!("{} is {what}", a.name) }
            })
            .collect()
    }

    /// The attachments as the runner and the relay take them, or what stops the run.
    pub(super) fn shell_attachments(&self) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>, Vec<(String, db::SqlRelay)>), String> {
        let (mut mongo, mut sql, mut relays) = (Vec::new(), Vec::new(), Vec::new());
        for a in self.t().map(|t| t.shell.attached.clone()).unwrap_or_default() {
            let Some(c) = self.conn(a.conn) else { return Err(format!("The connection of {} no longer exists. Detach it with the x on its chip.", a.name)) };
            let list = c.dbs.iter().find(|d| d.0 == a.db).and_then(|d| d.1.clone()).unwrap_or_default();
            // Attached connections are read-only unless the tab allows writes and the connection itself is unlocked.
            let read_only = !(a.allow_writes && c.writable);
            if let Some(db) = c.sql.clone() {
                let pg = db.dialect == db::sql::Dialect::Postgres;
                // A PostgreSQL connection belongs to one database, so the database picks the pool and the schema is the search_path.
                if pg {
                    sql.push(serde_json::json!({ "name": a.name, "db": a.db, "tables": a.tables, "dialect": "postgres" }));
                    relays.push((a.name.clone(), db::SqlRelay { db: db.at(&a.db), schema: if a.schema.is_empty() { PG_SCHEMA.into() } else { a.schema.clone() }, read_only, conn_id: Default::default() }));
                    continue;
                }
                sql.push(serde_json::json!({ "name": a.name, "db": a.db, "tables": list, "dialect": "mysql" }));
                relays.push((a.name.clone(), db::SqlRelay { db, schema: a.db.clone(), read_only, conn_id: Default::default() }));
            } else if c.client.is_some() {
                let uri = stored_uri(a.conn).map_err(uri_err)?;
                mongo.push(serde_json::json!({ "name": a.name, "uri": uri, "db": a.db, "proxy": self.shell_proxy(a.conn), "collections": list, "readOnly": read_only }));
            } else {
                return Err(format!("{} uses {}, which is not connected. Connect it in the sidebar, then run again.", a.name, c.meta.name));
            }
        }
        Ok((mongo, sql, relays))
    }

    /// Each attachment as the language server types it. Collections are typed from the field info of
    /// open tabs of that connection and database; tables from the schema asked for when attaching.
    pub(super) fn attached_infos(&self) -> Vec<AttachedInfo> {
        let Some(t) = self.t() else { return Vec::new() };
        t.shell
            .attached
            .iter()
            .filter_map(|a| {
                let c = self.conn(a.conn)?;
                let pg = c.meta.engine == db::sql::Engine::Postgres;
                let names: Vec<String> = if pg { a.tables.clone() } else { c.dbs.iter().find(|d| d.0 == a.db).and_then(|d| d.1.clone()).unwrap_or_default() };
                // Columns of a PostgreSQL attachment are not read yet, so its tables are typed loosely.
                let typed = if pg {
                    AttTyped::Sql(None)
                } else if c.meta.engine.is_sql() { AttTyped::Sql(self.sql_schema.get(&(a.conn, a.db.clone())).cloned()) } else { AttTyped::Mongo(self.fields_of(a.conn, &a.db)) };
                Some(AttachedInfo { name: a.name.clone(), names, typed })
            })
            .collect()
    }

    /// The Attach dialog.
    pub(super) fn attach_dialog(&mut self, ui: &mut egui::Ui, f: &mut AttachForm, keep: &mut bool) {
        ui.set_width(600.0);
        dialog_title(ui, "Attach a connection");
        ui.label(RichText::new("Its data becomes a handle in this shell's scripts").size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        let filter = ui.add(egui::TextEdit::singleline(&mut f.filter).hint_text("Filter saved connections").desired_width(f32::INFINITY));
        if !f.focused {
            filter.request_focus();
            f.focused = true;
        }
        ui.add_space(6.0);
        let q = f.filter.trim().to_lowercase();
        let rows: Vec<u64> = self.conns.iter().filter(|c| q.is_empty() || c.meta.name.to_lowercase().contains(&q) || c.meta.tag.to_lowercase().contains(&q)).map(|c| c.meta.id).collect();
        let mut pick = None;
        // Four whole rows of ROW_STEP, so the list never stops on half a row.
        const ROW_STEP: f32 = 42.0;
        egui::ScrollArea::vertical().max_height(4.0 * ROW_STEP).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = ROW_STEP - CONN_ROW_H;
            if rows.is_empty() {
                ui.label(RichText::new("No saved connection matches").color(FAINT()));
            }
            for id in &rows {
                let Some(c) = self.conn(*id) else { continue };
                if connection_row(ui, c, f.conn == Some(*id)).clicked() {
                    pick = Some(*id);
                }
            }
        });
        if let Some(id) = pick {
            f.conn = Some(id);
            f.db.clear();
            f.name_set = false;
            // The databases come with the connection, which also opens its SSH tunnel.
            if self.conn(id).is_some_and(|c| matches!(c.status, Status::Idle | Status::Failed(_))) {
                self.connect(id);
            }
        }
        let chosen = f.conn.and_then(|id| self.conn(id));
        let (status, dbs): (Option<&Status>, Vec<String>) = chosen.map_or((None, Vec::new()), |c| (Some(&c.status), c.dbs.iter().map(|d| d.0.clone()).collect()));
        let ready = status == Some(&Status::Ready);
        let conn_name = chosen.map(|c| c.meta.name.clone()).unwrap_or_default();
        if ready && !dbs.contains(&f.db) {
            // The tab's own database when it is the same connection, else the first one a user would look at.
            let own = self.t().filter(|t| Some(t.sel.conn) == f.conn).map(|t| t.sel.db.clone()).filter(|d| dbs.contains(d));
            let system = |d: &str| ["admin", "local", "config"].contains(&d) || db::sql::create::SYSTEM_SCHEMAS.contains(&d);
            f.db = own.or_else(|| dbs.iter().find(|d| !system(d)).cloned()).or_else(|| dbs.first().cloned()).unwrap_or_default();
            f.name_set = false;
        }
        let pg = chosen.is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres);
        // The schemas of a PostgreSQL database are listed once per picked database; they arrive in `shell_pg_listed`.
        if pg && ready && !f.db.is_empty() && f.asked != f.conn.map(|c| (c, f.db.clone())) {
            f.asked = f.conn.map(|c| (c, f.db.clone()));
            f.schemas.clear();
            f.schema.clear();
            if let (Some(conn), Some(sql)) = (f.conn, chosen.and_then(|c| c.sql.clone())) {
                self.worker.pg_shell_list(conn, sql, f.db.clone(), String::new());
            }
        }
        if !f.name_set {
            f.name = if f.db.is_empty() { String::new() } else { default_name(&f.db) };
        }
        let problem = (f.conn.is_some() && !f.name.is_empty() || f.name_set).then(|| name_problem(&f.name, &self.shell_taken())).flatten();
        ui.add_space(8.0);
        egui::Grid::new("attach-form").num_columns(2).spacing([12.0, 10.0]).min_col_width(FORM_LABEL_W + 30.0).show(ui, |ui| {
            form_label(ui, "Database");
            match status {
                Some(Status::Ready) => {
                    egui::ComboBox::from_id_salt("attach-db").selected_text(&f.db).width(220.0).show_ui(ui, |ui| {
                        menu_look(ui);
                        for d in &dbs {
                            if ui.selectable_label(f.db == *d, d).clicked() {
                                f.db = d.clone();
                                f.name_set = false;
                            }
                        }
                    });
                }
                Some(Status::Connecting) => {
                    ui.horizontal(|ui| {
                        spin(ui);
                        ui.label(RichText::new(format!("Connecting to {conn_name}")).color(MUTED()));
                    });
                }
                Some(Status::Failed(e)) => {
                    ui.label(RichText::new(format!("Could not connect: {e}")).color(ALERT()));
                }
                _ => {
                    ui.label(RichText::new("Pick a connection above").color(FAINT()));
                }
            }
            ui.end_row();
            if pg && ready {
                form_label(ui, "Schema");
                if f.schemas.is_empty() {
                    ui.horizontal(|ui| {
                        spin(ui);
                        ui.label(RichText::new("Listing schemas").color(MUTED()));
                    });
                } else {
                    egui::ComboBox::from_id_salt("attach-schema").selected_text(&f.schema).width(220.0).show_ui(ui, |ui| {
                        menu_look(ui);
                        for s in &f.schemas {
                            if ui.selectable_label(f.schema == *s, s).clicked() {
                                f.schema = s.clone();
                            }
                        }
                    });
                }
                ui.end_row();
            }
            form_label(ui, "Name in scripts");
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut f.name).font(egui::TextStyle::Monospace).desired_width(220.0).margin(egui::Margin::symmetric(8, 4));
                if ui.add_enabled(ready, edit).changed() {
                    f.name_set = true;
                }
                match &problem {
                    Some(p) => ui.label(RichText::new(p).size(TXT_SMALL).color(ALERT())),
                    None => ui.label(RichText::new("A valid JS name").size(TXT_SMALL).color(FAINT())),
                };
            });
            ui.end_row();
            form_label(ui, "Writes");
            ui.vertical(|ui| {
                ui.add_enabled(ready, egui::Checkbox::new(&mut f.writes, if conn_name.is_empty() { "Allow writes when the connection is unlocked in Quarry".to_string() } else { format!("Allow writes when {conn_name} is unlocked in Quarry") }));
                ui.add_space(2.0);
                ui.label(RichText::new("Off by default. Each connection keeps its own read-only lock and SSH tunnel. Attachments are saved with the tab and come back when it is reopened.").size(TXT_SMALL).color(FAINT()));
            });
            ui.end_row();
        });
        ui.add_space(4.0);
        let can = ready && !f.db.is_empty() && problem.is_none() && !f.name.is_empty() && (!pg || !f.schema.is_empty());
        let mut attach = false;
        dialog_actions(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                attach = ui.add_enabled(can, primary_button(ui, "Attach", PRIMARY())).clicked();
                if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    *keep = false;
                }
            });
        });
        if attach {
            if let (Some(conn), Some(t)) = (f.conn, self.tm()) {
                t.shell.attached.push(Attached { conn, db: f.db.clone(), name: f.name.clone(), allow_writes: f.writes, schema: if pg { f.schema.clone() } else { String::new() }, tables: Vec::new() });
                self.attach_load(conn, &f.db);
            }
            *keep = false;
        }
    }

    /// The answer of `pg_shell_list`: the schemas fill the open Attach dialog, and the tables go to the attachments of every
    /// tab that match, so a script run after this has its table helpers.
    pub(super) fn shell_pg_listed(&mut self, d: Document) {
        let (conn, database, schema) = (d.get_i64("conn").unwrap_or_default() as u64, d.get_str("database").unwrap_or_default().to_string(), d.get_str("schema").unwrap_or_default().to_string());
        if let Ok(e) = d.get_str("error") {
            return self.err_quiet(e.to_string(), Some(conn), None, format!("List schemas · {database}"));
        }
        let names = |k: &str| d.get_array(k).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
        if let Some(Modal::Attach(f)) = &mut self.modal {
            if f.conn == Some(conn) && f.db == database && schema.is_empty() {
                f.schemas = names("schemas");
                // Where a new database starts, else the first one a person would look at.
                if !f.schemas.contains(&f.schema) {
                    f.schema = if f.schemas.iter().any(|s| s == PG_SCHEMA) { PG_SCHEMA.to_string() } else { f.schemas.first().cloned().unwrap_or_default() };
                }
            }
        }
        if !schema.is_empty() {
            let tables = names("tables");
            for t in &mut self.tabs {
                for a in t.shell.attached.iter_mut().filter(|a| a.conn == conn && a.db == database && (a.schema == schema || a.schema.is_empty() && schema == PG_SCHEMA)) {
                    a.tables = tables.clone();
                }
            }
        }
    }
}

/// One saved connection in the Attach list: engine mark, name, engine and version, tag, and its state on the right.
/// A connection row of the Attach dialog; its list is sized from this.
const CONN_ROW_H: f32 = 40.0;

fn connection_row(ui: &mut egui::Ui, c: &Conn, active: bool) -> egui::Response {
    let (mark, product, color) = mark_of(c);
    let version = short_version(&c.server.version);
    let engine = if version.is_empty() { product.to_string() } else { format!("{product} {version}") };
    side_row(ui, CONN_ROW_H, 0.0, active, |p, r| {
        let mid = r.center().y;
        let sq = egui::Rect::from_center_size(egui::pos2(r.left() + 14.0, mid), egui::vec2(28.0, 28.0));
        p.rect_filled(sq, R_MD, tint(color, 45));
        p.text(sq.center(), egui::Align2::CENTER_CENTER, mark, FontId::monospace(9.0), color);
        let layout = |text: &str, fmt: egui::TextFormat| p.layout_job(egui::text::LayoutJob::single_section(text.to_string(), fmt));
        let name = layout(&c.meta.name, ui_fmt(TXT_TITLE, if active { BRIGHT() } else { TEXT() }, W_SEMIBOLD, 0.0));
        let mut x = sq.right() + 12.0;
        p.galley(egui::pos2(x, mid - name.size().y / 2.0 + optical_dy(&name)), name.clone(), BRIGHT());
        x += name.size().x + 10.0;
        // A small tinted label each, the engine first and the environment tag after it.
        let badge = |text: &str, fg: Color32, mono: bool, x: &mut f32| {
            let g = layout(text, if mono { mono_fmt(TXT_CAPTION, fg, 400.0) } else { ui_fmt(TXT_CAPTION, fg, 400.0, 0.0) });
            let rect = egui::Rect::from_center_size(egui::pos2(*x + g.size().x / 2.0 + 6.0, mid), egui::vec2(g.size().x + 12.0, 18.0));
            p.rect(rect, R_SM, tint(fg, 31), Stroke::new(1.0, tint(fg, 50)), egui::StrokeKind::Inside);
            p.galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, optical_dy(&g)), g, fg);
            *x = rect.right() + 6.0;
        };
        badge(&engine, color, true, &mut x);
        if !c.meta.tag.is_empty() {
            badge(&c.meta.tag, super::sidebar::tag_color(&c.meta.tag), false, &mut x);
        }
        let (state, c_state) = match &c.status {
            Status::Ready => ("", SUCCESS()),
            Status::Connecting => ("connecting", WARNING()),
            Status::Failed(_) => ("failed", ALERT()),
            Status::Idle => ("not connected", FAINT()),
        };
        if !state.is_empty() {
            let g = layout(state, mono_fmt(TXT_CAPTION, c_state, 400.0));
            p.galley(egui::pos2(r.right() - g.size().x - 6.0, mid - g.size().y / 2.0 + optical_dy(&g)), g, c_state);
        }
    })
}

/// The declarations of every attachment, for the language server. `main_sql` says the tab has no
/// SQL type aliases of its own to share.
pub(super) fn attached_decls(items: &[AttachedInfo], main_sql: bool) -> String {
    let schemas: Vec<HashMap<String, String>> = items.iter().map(|i| if let AttTyped::Mongo(f) = &i.typed { f.iter().map(|(c, f)| (c.clone(), ts_type(f))).collect() } else { HashMap::new() }).collect();
    // A table whose columns are not known yet still gets a handle, typed loosely.
    let tables: Vec<Vec<db::sql::TableCols>> = items
        .iter()
        .map(|i| match &i.typed {
            AttTyped::Sql(Some(s)) => s.tables.iter().map(|t| db::sql::TableCols { name: t.name.clone(), cols: t.cols.iter().map(|c| db::sql::TableCol { name: c.name.clone(), full_type: c.ty.clone(), nullable: c.nullable }).collect() }).collect(),
            _ => i.names.iter().map(|n| db::sql::TableCols { name: n.clone(), cols: Vec::new() }).collect(),
        })
        .collect();
    let kinds: Vec<(&str, db::AttachedKind)> = items
        .iter()
        .enumerate()
        .map(|(n, i)| (i.name.as_str(), if matches!(i.typed, AttTyped::Mongo(_)) { db::AttachedKind::Mongo { collections: &i.names, schemas: &schemas[n] } } else { db::AttachedKind::Sql { tables: &tables[n] } }))
        .collect();
    db::attached_globals(&kinds, !main_sql, &JS_RESERVED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_identifiers() {
        assert_eq!(default_name("app"), "app");
        assert_eq!(default_name("shop-prod"), "shop_prod");
        assert_eq!(default_name("2024 sales"), "_2024_sales");
        assert_eq!(default_name("class"), "class_");
        assert_eq!(default_name("---"), "conn");
        assert_eq!(default_name(""), "conn");
        assert!(name_problem(&default_name("my.db"), &[]).is_none());
    }

    #[test]
    fn names_are_checked() {
        let taken = vec![("db".to_string(), "a name the shell provides".to_string()), ("shop".to_string(), "used by another attached connection".to_string())];
        assert_eq!(name_problem("travels", &taken), None);
        assert_eq!(name_problem("$x_1", &taken), None);
        assert_eq!(name_problem("", &taken).as_deref(), Some("Enter a name."));
        assert!(name_problem("1abc", &taken).unwrap().starts_with("Use letters"));
        assert!(name_problem("my-db", &taken).unwrap().starts_with("Use letters"));
        assert!(name_problem("class", &taken).unwrap().contains("reserved word"));
        assert_eq!(name_problem("db", &taken).as_deref(), Some("db is already a name the shell provides."));
        assert_eq!(name_problem("shop", &taken).as_deref(), Some("shop is already used by another attached connection."));
    }

    #[test]
    fn versions_are_shortened() {
        assert_eq!(short_version("8.0.36"), "8.0");
        assert_eq!(short_version("11.4"), "11.4");
        assert_eq!(short_version(""), "");
    }

    #[test]
    fn declarations_follow_the_attachments() {
        let items = [
            AttachedInfo { name: "travels".into(), names: vec!["users".into(), "trips".into()], typed: AttTyped::Mongo(Vec::new()) },
            AttachedInfo { name: "shop".into(), names: vec!["orders".into()], typed: AttTyped::Sql(None) },
        ];
        let g = attached_decls(&items, false);
        assert!(g.contains("declare const travels: import(\"mongodb\").Db & Omit<{ users: import(\"mongodb\").Collection; trips: import(\"mongodb\").Collection }"), "{g}");
        assert!(g.contains("declare const shop: QuarryConn & Omit<{ orders: QuarryTable<Quarry_shop_Orders> }"), "{g}");
        assert!(g.contains("interface QuarryConn {"));
        assert!(!attached_decls(&items, true).contains("interface QuarryConn"));
    }
}
