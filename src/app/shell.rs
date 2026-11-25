//! The JS shell view and running scripts.

use super::*;
use quarry_core::action::{Action, ShellA};
use crate::ui::sql_complete::{Wants, analyze, split_statement};
use crate::app::library::snippet_strip::{self as snip, Src};
use crate::library::snippet::Lang;
use std::cell::RefCell;

pub(super) use quarry_core::action::shell::shell_ops;

/// The original's "Open query in NodeShell" script (renderer line 96080), with its collection
/// reference rule (getSnippetCollectionRef): names that are not plain words go through db.collection().
/// `chain` follows find(); a pasted shell command carries its own options, so it is left out there.
pub(super) fn query_script(coll: &str, filter: &str, chain: &str) -> String {
    let f = filter.trim().trim_end_matches(';');
    if db::is_shell(f) {
        return format!("async function run() {{\n\treturn await {f}\n}}");
    }
    let f = if f.is_empty() { "{}" } else { f };
    let expanded = db::expand_shortcuts(f);
    let f = expanded.as_deref().unwrap_or(f);
    format!("async function run() {{\n\treturn await {}.find({f}){chain}\n}}", collection_ref(coll))
}

pub(super) fn cursor_chain(q: &db::Query) -> String {
    let proj = q.projection.as_ref().map(|p| format!(".project({})", db::shell_text(&Bson::Document(p.clone()))));
    proj.unwrap_or_default() + &db::mongosh_chain(q)
}

pub(super) use crate::db::JS_RESERVED;

/// The Shell's reference: how scripts work, every name the runner provides, the main methods and
/// examples that can be loaded into the editor. Returns an example to load, if one was picked.
pub(super) fn shell_help(ui: &mut egui::Ui, coll: &str, writable: bool, sql: bool) -> Option<String> {
    if sql {
        return sql_help(ui, coll, writable);
    }
    let mut pick = None;
    let code = |s: &str| RichText::new(s).monospace().size(TXT_BODY).color(C_DATE());
    let head = |ui: &mut egui::Ui, s: &str| {
        ui.add_space(4.0);
        caption(ui, s);
    };
    egui::Frame::new().fill(PANEL()).corner_radius(R_MD).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        head(ui, "HOW IT WORKS");
        ui.label("Write one function and return what you want to see. The script runs with Bun and the official MongoDB Node.js driver, so its whole API is available.");
        ui.label(code("async function run() {\n  return await orders.find({ status: 'paid' }).toArray();\n}"));
        ui.label(RichText::new("A returned cursor is cut to 100 documents. Logs from console.log/warn/error appear above the result. Cmd+Enter runs, Stop ends a long script.").color(MUTED()));
        if !writable {
            ui.label(RichText::new("This connection is read-only: insert, update, delete, drop, index changes, $out/$merge and other write commands are refused. Unlock writes in the top bar to allow them.").color(SUCCESS()));
        }
        head(ui, "NAMES YOU CAN USE");
        egui::Grid::new("shell-names").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (n, what) in [
                ("db", "the current database (Db): db.collection('name'), db.command({ ping: 1 }), db.listCollections()"),
                ("<collection>", "every collection whose name is a plain word, e.g. orders.find()"),
                ("<attached>", "each connection attached in Connections, by its name: travels.users.find() for MongoDB, shop`SELECT 1` or shop.orders.find() for SQL"),
                ("ObjectId(\"…\")", "an ObjectId, with or without new"),
                ("ISODate(\"…\")", "a Date from an ISO string"),
                ("console.log / warn / error, log", "print to the log"),
                ("randomId(n), UUIDv4(), UUIDv1()", "random ids"),
                ("_ , dayjs", "lodash and dayjs, always available like in the original shell"),
                ("packages", "every package added in Packages is ready by name: date-fns as dateFns, lodash as lodash and _"),
                ("require('pkg'), import('pkg')", "load a package another way, or a part of it"),
            ] {
                ui.label(code(n));
                ui.label(what);
                ui.end_row();
            }
        });
        head(ui, "COLLECTION METHODS");
        ui.label(RichText::new(COLL_METHODS.join("  ·  ")).monospace().size(TXT_SMALL).color(TEXT()));
        head(ui, "CURSOR METHODS (after find / aggregate)");
        ui.label(RichText::new(CURSOR_METHODS.join("  ·  ")).monospace().size(TXT_SMALL).color(TEXT()));
        head(ui, "EXAMPLES  ·  click Use to load one");
        let examples = [
            ("Find with a filter", format!("async function run() {{\n  return await {coll}.find({{ status: 'paid' }}).sort({{ _id: -1 }}).limit(20).toArray();\n}}")),
            ("Count", format!("async function run() {{\n  return await {coll}.countDocuments({{}});\n}}")),
            ("Group and count", format!("async function run() {{\n  return await {coll}.aggregate([\n    {{ $group: {{ _id: '$status', n: {{ $sum: 1 }} }} }},\n    {{ $sort: {{ n: -1 }} }}\n  ]).toArray();\n}}")),
            ("Distinct values", format!("async function run() {{\n  return await {coll}.distinct('status');\n}}")),
            ("Loop and log", format!("async function run() {{\n  for await (const d of {coll}.find().limit(5)) {{\n    console.log(d._id);\n  }}\n  return 'done';\n}}")),
            ("By id", format!("async function run() {{\n  return await {coll}.findOne({{ _id: ObjectId(\"\") }});\n}}")),
            ("Use lodash (built in as _)", format!("async function run() {{\n  const docs = await {coll}.find().limit(100).toArray();\n  return _.countBy(docs, 'status');\n}}")),
            ("Update (needs writes unlocked)", format!("async function run() {{\n  return await {coll}.updateMany({{ status: 'pending' }}, {{ $set: {{ status: 'paid' }} }});\n}}")),
        ];
        for (title, ex) in examples {
            ui.horizontal(|ui| {
                if ui.button("Use").clicked() {
                    pick = Some(ex.clone());
                }
                ui.label(RichText::new(title).color(BRIGHT()));
            });
        }
        ui.hyperlink_to("MongoDB Node.js driver reference", "https://www.mongodb.com/docs/drivers/node/current/");
    });
    pick
}

pub(super) fn code_menu(ui: &mut egui::Ui, pick: &mut Option<db::Lang>) {
    ui.scope(|ui| {
        quiet_look(ui);
        ui.menu_button("Copy as code", |ui| {
            menu_look(ui);
            for l in db::Lang::ALL {
                if ui.button(l.name()).clicked() {
                    *pick = Some(l);
                    ui.close();
                }
            }
        });
    });
}

/// One click adds these from the empty Packages list. Not lodash, dayjs or uuid: those are always loaded.
const PACKAGE_SUGGESTIONS: [&str; 4] = ["date-fns", "zod", "nanoid", "ms"];

/// The Packages popover: an add field, the added packages with the names scripts use, and suggestions when empty.
fn packages_popover(ui: &mut egui::Ui, packages: &mut Vec<String>, new_pkg: &mut String) {
    popover_body(ui, 340.0, |ui| {
        popover_head(ui, "Packages", "npm", "Ready in every script by the name on the right.");
        let spec = new_pkg.trim().to_string();
        let name = db::package_spec(&spec).0;
        let problem = if db::PINNED_PACKAGES.contains(&name.as_str()) {
            Some(format!("{name} is built in, at the version the shell needs."))
        } else if packages.contains(&spec) {
            Some(format!("{spec} is already added."))
        } else {
            None
        };
        let valid = problem.is_none() && db::valid_package(&spec);
        let field = ui.make_persistent_id("shell-new-pkg");
        let mut add = None;
        well_field(ui, field, FieldIcon::Plus, problem.is_some().then(ALERT), |ui| {
            let w = ui.available_width() - if spec.is_empty() { 100.0 } else { 60.0 };
            let r = ui.add(field_edit(new_pkg, field, "Add a package, like dayjs@1.11", w, egui::TextStyle::Monospace));
            if field_button(ui, &r, "Add", 50.0, valid, spec.is_empty()) {
                add = Some(spec.clone());
            }
        });
        if let Some(msg) = &problem {
            field_note(ui, msg, ALERT());
        }
        ui.add_space(12.0);

        if packages.is_empty() {
            empty_state(ui, paint_cube, "No packages yet", "", |ui| {
                ui.add_space(10.0);
                let gap = 6.0;
                let total = PACKAGE_SUGGESTIONS.iter().map(|s| add_chip_w(ui, s)).sum::<f32>() + gap * (PACKAGE_SUGGESTIONS.len() - 1) as f32;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    ui.add_space(((ui.available_width() - total) / 2.0 - gap).max(0.0));
                    for s in PACKAGE_SUGGESTIONS {
                        if add_chip(ui, s).clicked() {
                            add = Some(s.to_string());
                        }
                    }
                });
            });
        } else {
            section_head(ui, "ADDED", None);
            // A package Bun has not installed yet is fetched on the next run. Checked at most once a
            // second, so an open popover does not touch the disk every frame.
            let cache = ui.make_persistent_id("pkg-installed");
            let now = ui.input(|i| i.time);
            let installed: Vec<bool> = match ui.data(|d| d.get_temp::<(f64, Vec<String>, Vec<bool>)>(cache)) {
                Some((at, list, inst)) if now - at < 1.0 && list == *packages => inst,
                _ => {
                    let modules = config_dir().join("shell").join("node_modules");
                    let inst: Vec<bool> = packages.iter().map(|p| modules.join(db::package_spec(p).0).exists()).collect();
                    ui.data_mut(|d| d.insert_temp(cache, (now, packages.clone(), inst.clone())));
                    inst
                }
            };
            let mut drop = None;
            for (i, p) in packages.iter().enumerate() {
                let (row, hovered) = pop_row(ui, POP_ROW_H);
                let (rect, painter) = (row.rect, ui.painter().clone());
                paint_cube(&painter, egui::pos2(rect.left() + 6.0, rect.center().y), MUTED());
                let (pkg, ver) = match p.rfind('@') {
                    Some(i) if i > 0 => (&p[..i], &p[i..]),
                    _ => (p.as_str(), ""),
                };
                let mut job = egui::text::LayoutJob::default();
                job.append(pkg, 0.0, mono_fmt(TXT_BODY, BRIGHT(), 400.0));
                job.append(ver, 0.0, mono_fmt(TXT_BODY, FAINT(), 400.0));
                galley_at(&painter, rect.left() + 22.0, rect.center().y, painter.layout_job(job), BRIGHT());
                let right = rect.right() - if hovered { CLOSE_W + 6.0 } else { 0.0 };
                let name = db::package_spec(p).0;
                if installed[i] {
                    let alias = if name == "lodash" { format!("{}, _", package_ident(p)) } else { package_ident(p) };
                    tag_at(&painter, right, rect.center().y, &alias, C_DATE());
                } else {
                    let g = painter.layout_no_wrap("installs on next run".into(), FontId::proportional(TXT_CAPTION), FAINT());
                    galley_at(&painter, right - g.size().x, rect.center().y, g, FAINT());
                }
                if hovered && row_close(ui, rect, rect.center().y, "Remove") {
                    drop = Some(i);
                }
            }
            if let Some(i) = drop {
                packages.remove(i);
            }
        }
        if let Some(p) = add.filter(|p| !packages.contains(p)) {
            packages.push(p);
            new_pkg.clear();
        }
        popover_foot(ui, Some(paint_shield), &["Bun installs these on the next run.", "Install scripts never run unless you trust the package."]);
    });
}

/// How a SQL script names a table: the plain word, else through table("...").
fn default_table_ref(coll: &str) -> String {
    if coll.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && coll.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !JS_RESERVED.contains(&coll) {
        coll.to_string()
    } else {
        format!("table({})", serde_json::Value::String(coll.into()))
    }
}

pub(super) fn default_script(coll: &str, sql: bool) -> String {
    if sql {
        if coll.is_empty() {
            return "async function run() {\n  // sql`...` runs SQL; ${values} are sent as escaped parameters, never pasted as text.\n  return await sql`SELECT 1 AS one`;\n}".into();
        }
        let r = default_table_ref(coll);
        // A name that is not a plain word cannot sit unquoted in SQL, and backticks would end the template, so it gets the helper.
        if r != coll {
            return format!("async function run() {{\n  // Table helpers: find, findOne, count, insert, update, delete. sql`...` runs any SQL.\n  return await {r}.find({{}}, {{ limit: 20 }});\n}}");
        }
        return format!(
            "async function run() {{\n  // sql`...` runs SQL; ${{values}} are sent as escaped parameters, never pasted as text.\n  const rows = await sql`SELECT * FROM {coll} LIMIT 20`;\n  // Or the table helpers: {r}.find({{ status: 'open' }}, {{ limit: 20 }}), {r}.findOne(..), {r}.count(..)\n  return rows;\n}}"
        );
    }
    let target = collection_ref(coll);
    format!("async function run() {{\n  return await {target}.find({{}}).limit(20).toArray();\n}}")
}

/// A fixed line height, so the gutter numbers line up with the text.
const LINE_H: f32 = 20.0;
/// Height of the Console and Queries panel under the result, tab row included.
const SHELL_PANEL_H: f32 = 150.0;

/// The panel under the result: the script's console output, or every query it ran. Returns the tab picked, if any.
fn shell_panel(ui: &mut egui::Ui, sh: &ShellTab, conn: &str, kinds: &[(String, bool, Color32)]) -> Option<bool> {
    let mut pick = None;
    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 6 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(SHELL_PANEL_H - 12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let (n_log, n_q) = (sh.logs.len() + sh.logs_dropped, sh.queries.len());
            if view_tab(ui, &format!("Console  {n_log}"), !sh.queries_tab).clicked() {
                pick = Some(false);
            }
            if view_tab(ui, &format!("Queries  {n_q}"), sh.queries_tab).clicked() {
                pick = Some(true);
            }
        });
        let line = ui.min_rect().bottom();
        ui.painter().hline(ui.max_rect().x_range().expand(12.0), line, Stroke::new(1.0, LINE()));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().id_salt("shell-panel").max_height(SHELL_PANEL_H - 50.0).auto_shrink([false, false]).show(ui, |ui| {
            let mono = |s: String, c: Color32| RichText::new(s).font(FontId::monospace(TXT_SMALL)).color(c);
            if !sh.queries_tab {
                if sh.logs.is_empty() {
                    ui.label(mono("Nothing printed".into(), FAINT()));
                }
                if sh.logs_dropped > 0 {
                    ui.label(mono(format!("{} earlier lines not kept", sh.logs_dropped), FAINT()));
                }
                for (level, text) in &sh.logs {
                    let c = match level.as_str() {
                        "error" => ALERT(),
                        "warn" => WARNING(),
                        "info" => PRIMARY(),
                        _ => TEXT(),
                    };
                    ui.label(mono(text.clone(), c));
                }
                return;
            }
            if sh.queries.is_empty() {
                ui.label(mono("No queries yet".into(), FAINT()));
            }
            // The same text run again (a loop) is one line with a count and the time added up.
            let mut rows: Vec<(&db::ShellQuery, usize, u128)> = Vec::new();
            for q in &sh.queries {
                match rows.iter_mut().find(|r| r.0.text == q.text && r.0.conn == q.conn) {
                    Some(r) => (r.1, r.2) = (r.1 + 1, r.2 + q.ms),
                    None => rows.push((q, 1, q.ms)),
                }
            }
            // One grid, so the query text of every connection starts at the same x.
            egui::Grid::new("shell-queries").num_columns(5).spacing([12.0, 3.0]).show(ui, |ui| {
                for (q, n, ms) in rows {
                    let name = if q.conn.is_empty() { conn.to_string() } else { q.conn.clone() };
                    let (is_sql, color) = kinds.iter().find(|k| k.0 == name).map_or((false, PRIMARY()), |k| (k.1, k.2));
                    ui.label(mono(name, color));
                    ui.label(mono(if is_sql { "sql".into() } else { String::new() }, FAINT()));
                    let one_line: String = q.text.split_whitespace().collect::<Vec<_>>().join(" ");
                    let cut = if one_line.chars().count() > 70 { format!("{}…", one_line.chars().take(70).collect::<String>()) } else { one_line };
                    ui.label(mono(cut, TEXT())).on_hover_text(&q.text);
                    ui.label(mono(q.params.clone(), FAINT()));
                    let (text, c) = match (&q.error, q.rows, q.affected) {
                        (Some(e), ..) => (e.clone(), ALERT()),
                        _ if n > 1 => (format!("×{n}   {ms} ms"), MUTED()),
                        (_, Some(r), _) => (format!("{} {}  ·  {ms} ms", group(&r.to_string()), if r == 1 { "row" } else { "rows" }), MUTED()),
                        (_, _, Some(a)) => (format!("{} affected  ·  {ms} ms", group(&a.to_string())), MUTED()),
                        _ => (format!("{ms} ms"), MUTED()),
                    };
                    ui.label(mono(text, c));
                    ui.end_row();
                }
            });
        });
    });
    pick
}

/// The Shell's reference for a SQL tab.
fn sql_help(ui: &mut egui::Ui, table: &str, writable: bool) -> Option<String> {
    let mut pick = None;
    let code = |s: &str| RichText::new(s).monospace().size(TXT_BODY).color(C_DATE());
    egui::Frame::new().fill(PANEL()).corner_radius(R_MD).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        caption(ui, "HOW IT WORKS");
        ui.label("Write one function and return what you want to see. The script runs with Bun and has your file and network access, but it gets no database driver and no credentials: every query goes through this app, on one connection for the whole run, so @variables and transactions last until the script ends. While writes are locked each query passes Quarry's read-only check. That check guards queries, it is not a sandbox for the script.");
        ui.label(code("async function run() {\n  const rows = await sql`SELECT * FROM orders WHERE status = ${'paid'}`;\n  return rows;\n}"));
        ui.label(RichText::new("Values in ${} become ? placeholders ($1, $2 on PostgreSQL, where ? is an operator) and are never spliced into the text. Whole numbers beyond 2^53 come back as BigInt, and unsigned values above 9223372036854775807 come back as strings (send them back as a string or BigInt). An array becomes a list for IN (${ids}). Cmd+Enter runs, Stop interrupts the running statement. Locking or unlocking writes takes effect on the next run, not in a script that is already running.").color(MUTED()));
        if !writable {
            ui.label(RichText::new("This connection is read-only: only reading statements run, and every write is refused. Unlock writes in the top bar to allow them.").color(SUCCESS()));
        }
        caption(ui, "NAMES YOU CAN USE");
        egui::Grid::new("sql-shell-names").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (n, what) in [
                ("sql`...`", "run a statement; returns the rows as objects (writes return an empty list with .affected and .insertId; on PostgreSQL an INSERT ... RETURNING returns its rows with .affected, and .insertId is the first returned column of a single row, undefined without RETURNING; table.insert(row, { returning: true }) or { returning: ['id'] } returns the written rows and sets .insertId to the first column of a single row)"),
                ("sql.unsafe(text, params)", "the same with text you built yourself and ? placeholders ($1, $2 on PostgreSQL)"),
                ("<table>", "every table of the database whose name is a plain word, e.g. orders.find()"),
                ("table('name')", "any table by its name, also names that are not plain words (table('name', 'schema') on PostgreSQL)"),
                ("<attached>", "each connection attached in Connections, by its name: travels.users.find() for MongoDB, shop`SELECT 1` or shop.orders.find() for SQL"),
                ("console.log / warn / error, log", "print to the Console"),
                ("_ , dayjs, packages", "lodash and dayjs, and every package added in Packages by name"),
            ] {
                ui.label(code(n));
                ui.label(what);
                ui.end_row();
            }
        });
        caption(ui, "TABLE METHODS");
        ui.label(RichText::new("find(where?, { select, orderBy, limit, offset })  ·  findOne  ·  count  ·  insert(row or rows, { returning } on PostgreSQL)  ·  update(where, set)  ·  delete(where)").monospace().size(TXT_SMALL).color(TEXT()));
        ui.label(RichText::new("where: { status: 'paid' } is =, null is IS NULL, a list is IN, and { total: { gt: 100, lte: 500 } } compares (gt, gte, lt, lte, ne, like, in). where must be a plain object, and a value of undefined or an empty condition is an error. update and delete need a where; only {} means every row.").color(MUTED()));
        caption(ui, "EXAMPLES  ·  click Use to load one");
        let examples = [
            ("Rows of a table", format!("async function run() {{\n  return await {table}.find({{}}, {{ orderBy: '-id', limit: 20 }});\n}}")),
            ("Count", format!("async function run() {{\n  return await {table}.count();\n}}")),
            ("SQL with a value", format!("async function run() {{\n  const id = 1;\n  return await sql`SELECT * FROM {table} WHERE id = ${{id}}`;\n}}")),
            ("Update (needs writes unlocked)", format!("async function run() {{\n  return await {table}.update({{ id: 1 }}, {{ id: 1 }});\n}}")),
        ];
        for (title, ex) in examples {
            ui.horizontal(|ui| {
                if ui.button("Use").clicked() {
                    pick = Some(ex.clone());
                }
                ui.label(RichText::new(title).color(BRIGHT()));
            });
        }
    });
    pick
}

impl App {
    pub(super) fn open_in_shell(&mut self, code: String) {
        self.core.open_in_shell(code);
    }

    /// Runs the current tab's script, after the write confirm when it may write.
    pub(super) fn run_shell_script(&mut self) {
        if let Some(tab) = self.t().map(|t| t.uid) {
            self.exec(Action::Shell(ShellA::Run { tab }));
        }
    }

    /// Runs the current tab's script without the confirm.
    pub(super) fn start_shell(&mut self) {
        if let Some(tab) = self.t().map(|t| t.uid) {
            self.exec(Action::Shell(ShellA::Start { tab }));
        }
    }

    pub(super) fn shell_proxy(&self, conn: u64) -> serde_json::Value {
        self.core.shell_proxy(conn)
    }

    pub(super) fn shell_view(&mut self, ui: &mut egui::Ui) {
        // The page draws before the windows over it, so a Cmd+Enter meant for a dialog reaches it first.
        let covered = self.window_open();
        let writable = self.sel_writable();
        let sql = self.t().is_some_and(|t| t.sql.is_some());
        let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        let (conn_name, db_name) = self.t().and_then(|t| self.conn(t.sel.conn).map(|c| (c.meta.name.clone(), t.sel.db.clone()))).unwrap_or_default();
        let own_pill = self.own_pill(&conn_name, &db_name, !writable);
        let att_pills = self.attached_pills();
        let kinds = self.shell_kinds();
        let (mut detach, mut open_attach) = (None, false);
        if sql {
            self.core.sql_columns_ask(false);
        }
        let history = self.t().and_then(|t| self.core.prefs.shell_histories.get(&format!("{}/{}", t.sel.conn, t.sel.db)).cloned()).unwrap_or_default();
        let mut packages = self.core.prefs.shell_packages.clone();
        let mut pkg_names: Vec<String> = packages.iter().map(|p| package_ident(p)).collect();
        if packages.iter().any(|p| db::package_spec(p).0 == "lodash") {
            pkg_names.push("_".into());
        }
        let mut new_pkg = std::mem::take(&mut self.core.new_pkg);
        let pick = self.pick_data(ui.ctx(), PickKind::Shell);
        let (mut pick_acts, mut demo_pick) = (Vec::new(), None);
        let colls: Vec<String> = self.t().map(|t| self.tables_of(&t.sel)).unwrap_or_default();
        let known = if sql { Vec::new() } else { self.db_fields() };
        // What SQL templates in the script complete from: the tab's database and its looked up values.
        let sql_cx = sql.then(|| (self.core.tabs[self.core.cur].sel.conn, self.core.tabs[self.core.cur].sel.db.clone(), self.sel_schema(&self.core.tabs[self.core.cur].sel)));
        let scope = sql_cx.as_ref().map(|(c, d, s)| Self::sql_scope(&self.core.conns, &self.core.sql_schema, &self.core.views, *c, d, s, None));
        let none = Fields::new();
        let values = sql_cx.as_ref().and_then(|(c, d, s)| self.core.sql_values.get(&(*c, ns_key(d, s)))).unwrap_or(&none);
        let strip = self.strip_of(Src::Shell);
        let (mut make_default, mut make_value) = (false, None);
        let (t, ac) = (&mut self.core.tabs[self.core.cur], &mut self.core.ac);
        if t.shell.code.is_empty() {
            t.shell.code = default_script(&coll, sql);
        }
        let running = t.shell.started;
        let uid = t.uid;
        // Clicks become actions, run after drawing.
        let mut acts: Vec<ShellA> = Vec::new();
        let (mut run, mut stop, mut editor, mut fmt) = (false, false, None, false);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let r = run_stop(ui, "⌘↵", running);
            if running.is_some() {
                stop = r.on_hover_text("End the script and interrupt its running statement").clicked();
            } else {
                run = r.on_hover_text("Cmd+Enter").clicked();
            }
            // With the library picker open, Cmd+Enter runs the script picked there, not this one.
            run |= running.is_none() && !covered && !egui::Popup::is_any_open(ui.ctx()) && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
            let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), Sense::hover());
            ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, LINE()));
            ui.label(RichText::new("Connections").color(MUTED()));
            // The pills share what the Attach button and the Packages, History and Help buttons on the right leave.
            let text_w = |t: &str| ui.painter().layout_no_wrap(t.to_string(), FontId::proportional(TXT_BODY), TEXT()).size().x;
            let pad = ui.spacing().button_padding.x * 2.0 + ui.spacing().item_spacing.x;
            let right = text_w("Format") + text_w("Help") + text_w("History") + text_w(&format!("Packages {}", packages.len())) + text_w(&format!("Library {}", pick.count)) + 5.0 * pad + 110.0;
            let n = 1 + att_pills.len();
            let pill_w = ((ui.available_width() - right) / n as f32 - ui.spacing().item_spacing.x).max(120.0);
            conn_pill(ui, ui.make_persistent_id("pill-own"), &own_pill, false, pill_w);
            for (i, p) in att_pills.iter().enumerate() {
                if conn_pill(ui, ui.make_persistent_id(("pill", i)), p, true, pill_w) {
                    detach = Some(i);
                }
            }
            open_attach = attach_button(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                fmt = quiet(ui, "Format").on_hover_text("Indent the script by its brackets and break long lines (Shift+Alt+F)").clicked();
                if ui.selectable_label(t.shell.help, "Help").on_hover_text("What the shell can do, with examples").clicked() {
                    acts.push(ShellA::Help { tab: uid, on: !t.shell.help });
                }
                ui.menu_button("History", |ui| {
                    menu_look(ui);
                    ui.set_max_width(520.0);
                    if ui.button("Reset to the starter script").clicked() {
                        acts.push(ShellA::Code { tab: uid, code: default_script(&coll, sql) });
                        ui.close();
                    }
                    ui.separator();
                    if history.is_empty() {
                        ui.label(RichText::new("Nothing run yet").color(MUTED()));
                    }
                    for h in &history {
                        let first: String = h.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("async function")).take(1).collect();
                        if ui.button(RichText::new(first.chars().take(80).collect::<String>()).monospace()).on_hover_text(h.as_str()).clicked() {
                            acts.push(ShellA::Code { tab: uid, code: h.clone() });
                            ui.close();
                        }
                    }
                });
                sticky_menu(ui, format!("Packages {}", packages.len()), |ui| packages_popover(ui, &mut packages, &mut new_pkg));
                demo_pick = Some(picker_button(ui, &pick, false, &mut pick_acts));
            });
        });
        ui.add_space(8.0);
        let mut use_example = None;
        if t.shell.help {
            egui::ScrollArea::vertical().id_salt("shell-help").max_height(ui.available_height() * 0.4).auto_shrink([false, true]).show(ui, |ui| {
                use_example = shell_help(ui, &if sql { default_table_ref(&coll) } else { collection_ref(&coll) }, writable, sql);
            });
            ui.add_space(8.0);
        }
        if let Some(s) = &strip {
            let was = t.snip.filled;
            make_default = snip::strip_ui(ui, s, &mut t.snip, true);
            if t.snip.filled != was {
                acts.push(ShellA::Filled { tab: uid, on: t.snip.filled });
                t.snip.filled = was;
            }
        }
        let filled_view = strip.is_some() && t.snip.filled;
        // The editor: line numbers, token colors, and the same frame for both engines.
        let id = ui.make_persistent_id("shell-editor");
        let focused = ui.memory(|m| m.has_focus(id));
        // Read before the list consumes it, so a late server answer cannot reopen a dismissed list.
        let esc = ui.input(|i| i.key_pressed(Key::Escape));
        // The editor keeps three lines, and the results below it the Console, the header and three rows.
        let (lo, hi) = (3.0 * LINE_H + 24.0, ui.available_height() - SHELL_PANEL_H - 3.0 * ROW_H - TABLE_HEAD_H - 70.0);
        let pref = self.core.prefs.shell_editor_h;
        let custom = pref > 0.0;
        let h = if custom { pref.clamp(lo, hi.max(lo)) } else { (t.shell.code.split('\n').count() as f32 * LINE_H + 24.0).clamp(110.0, (ui.available_height() * 0.4).max(110.0)) };
        let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            crate::ui::editor::memo_galley(ui, if sql { 3 } else { 2 }, buf.as_str(), wrap, || {
                let mut job = if sql { shell_ls::js_sql_job(buf.as_str()) } else { js_job(buf.as_str()) };
                for sec in &mut job.sections {
                    sec.format.line_height = Some(LINE_H);
                }
                if buf.as_str().contains("{{") {
                    let holes: Vec<_> = crate::library::snippet::parse(buf.as_str(), Lang::Shell).holes.into_iter().map(|h| h.span).collect();
                    crate::ui::editor::tint_holes(&mut job, &holes);
                }
                job
            })
        };
        // Inside sql`...` the suggestions are SQL ones; elsewhere the built-in and server ones stay.
        let tail = if scope.is_some() && focused { shell_ls::template_tail(ui.ctx(), id, &t.shell.code) } else { String::new() };
        let (meta, need): (RefCell<Option<(String, String)>>, RefCell<Option<crate::ui::sql_complete::Need>>) = Default::default();
        let compute = |b: &str, force: bool| -> Option<(usize, Vec<Sug>)> {
            let tpl = scope.as_ref().and_then(|_| sql_at(b, b.len()));
            let (Some(tpl), Some(scope)) = (tpl, &scope) else { return suggest_js(b, &known, &colls, &pkg_names) };
            let (sb, full) = split_statement(&sql_text(b, &tpl, b.len()).0, &tail, scope.dialect);
            let o = analyze(&sb, &full, scope, values, force)?;
            *need.borrow_mut() = o.need.clone();
            *meta.borrow_mut() = Some((o.title.clone(), o.foot.clone()));
            (!o.sugs.is_empty()).then(|| (sb[o.from..].chars().count(), o.sugs))
        };
        if let (Some(scope), true) = (&scope, std::env::var("QUARRY_TEST_SQL").is_ok_and(|v| v == "shellac")) {
            shell_ls::shell_ac_demo(ui, id, &t.shell.code, ac, scope, values);
        }
        shell_ls::shell_sig_demo(ui, id, &t.shell.code, ac, &mut self.js);
        let mut sql_need = None;
        if fmt || crate::ui::editor::format_key(ui, id) {
            crate::ui::editor::format_in(ui.ctx(), id, &mut t.shell.code);
        }
        if filled_view {
            let out = snip::fill_tab(t, &t.shell.code, Src::Shell, Lang::Shell, true);
            snip::preview_ui(ui, out, 11, |c| if sql { shell_ls::js_sql_job(c) } else { js_job(c) });
        }
        let framed = (!filled_view).then(|| egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("shell-ed").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                crate::ui::vim::demo(ui, id, &t.shell.code);
                let (out, picked) = crate::ui::vim::wrap_with(ui, id, &mut t.shell.code, |ui, code| {
                    ac_field(ui, id, code, ac, &compute, |ui, text, _open| {
                    egui::TextEdit::multiline(text)
                        .id(id)
                        .font(FontId::monospace(TXT_SMALL))
                        .frame(gutter_frame())
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, if custom { h } else { 96.0 }))
                        .lock_focus(true)
                        .layouter(&mut layout)
                        .show(ui)
                    })
                });
                out.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Shell editor"));
                // A script the Cmd+K form filled reads its braces as placeholders again once the person edits it.
                if out.response.changed() {
                    t.snip.off = false;
                }
                out.response.context_menu(|ui| {
                    menu_list(ui);
                    let picked = egui::TextEdit::load_state(ui.ctx(), id).and_then(|st| st.cursor.char_range()).and_then(|r| {
                        let (a, b) = (r.primary.index.0, r.secondary.index.0);
                        let (a, b) = (crate::ui::editor::byte_at(&t.shell.code, a.min(b)), crate::ui::editor::byte_at(&t.shell.code, a.max(b)));
                        snip::literal_at(&t.shell.code, a..b.max(a + 1), Lang::Shell)
                    });
                    let mut acts = Vec::new();
                    if let Some(r) = picked {
                        menu_act(ui, &mut acts, r, |ui| ui.button("Make it a value\u{2026}"));
                    } else {
                        menu_act(ui, &mut acts, 0..0, |ui| ui.add_enabled(false, egui::Button::new("Make it a value\u{2026}")).on_disabled_hover_text("Select a string, a number or an ISODate"));
                    }
                    make_value = acts.pop();
                });
                if ac.id == Some(id.value()) && !ac.items.is_empty() && let Some((title, foot)) = meta.borrow_mut().take() {
                    (ac.title, ac.foot) = (title, foot);
                }
                sql_need = need.borrow_mut().take();
                let caret = out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0);
                let cur_line = caret.map_or(0, |c| t.shell.code.chars().take(c).filter(|ch| *ch == '\n').count() + 1);
                paint_gutter(ui, &out, Some(cur_line));
                let hover = out.response.hover_pos().and_then(|p| {
                    // The character under the pointer, not just the nearest gap between two.
                    let rel = p - out.galley_pos;
                    let gap = out.galley.cursor_from_pos(rel).index.0;
                    let at = |c: usize| out.galley.pos_from_cursor(egui::text::CCursor::new(c));
                    let i = if rel.x < at(gap).min.x { gap.checked_sub(1)? } else { gap };
                    let (cell, next) = (at(i), at(i + 1));
                    (rel.x >= cell.min.x && rel.x < next.min.x && rel.y >= cell.min.y && rel.y < cell.max.y).then_some((i, p))
                });
                editor = Some(shell_ls::EditorFrame {
                    id,
                    edited: caret.filter(|_| out.response.changed()),
                    esc,
                    picked,
                    focused: out.response.has_focus(),
                    caret,
                    caret_pos: caret.map(|c| out.galley_pos + out.galley.pos_from_cursor(egui::text::CCursor::new(c)).left_top().to_vec2()),
                    hover,
                });
            });
        }));
        let bar = splitter(ui);
        let saved = framed.is_some_and(|f| resize_editor(&bar, &mut self.core.prefs.shell_editor_h, f.response.rect.height(), lo, hi));
        ui.add_space(2.0);
        if let Some(e) = &t.shell.error {
            egui::Frame::new().fill(tint(ALERT(), 20)).corner_radius(R_MD).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(e).monospace().color(ALERT()));
            });
            ui.add_space(8.0);
        }
        // A child Ui, not a scope: the closure would need both the tab and self.
        let outer = ui;
        let mut result_ui = outer.new_child(egui::UiBuilder::new().id_salt("shell-result").max_rect(outer.available_rect_before_wrap()).layout(*outer.layout()));
        if t.shell.started.is_some() {
            // disable() also halves the opacity.
            result_ui.disable();
        }
        let ui = &mut result_ui;
        let has_table = !t.shell.docs.is_empty() && !t.shell.raw;
        let (mut export, mut page_to, mut too_big) = (false, None, None);
        if t.shell.value.is_some() || !t.shell.docs.is_empty() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let unit = if sql { "rows" } else { "documents" };
                let what = match (t.shell.docs.len(), t.shell.count) {
                    (0, Some(n)) => format!("{n} items"),
                    (0, None) => t.shell.value.as_ref().map_or("", db::type_name).to_string(),
                    (n, _) => format!("{} {unit}", group(&n.to_string())),
                };
                ui.label(semibold(RichText::new(what).font(FontId::monospace(TXT_SMALL)).color(BRIGHT())));
                if t.shell.cut {
                    ui.label(RichText::new(format!("cut at {} GB", db::SHELL_MAX >> 30)).font(FontId::monospace(TXT_SMALL)).color(WARNING())).on_hover_text(
                        "The script returned more than the Shell keeps, so only the first documents are here. To read the rest, return fewer fields with a projection, page with skip and limit, or return the cursor rather than toArray() so less is read at all.",
                    );
                }
                if !t.shell.queries.is_empty() {
                    let mut conns: Vec<&str> = t.shell.queries.iter().map(|q| q.conn.as_str()).collect();
                    conns.sort_unstable();
                    conns.dedup();
                    let n = t.shell.queries.len();
                    let text = if conns.len() > 1 { format!("{n} queries on {} connections", conns.len()) } else { format!("{n} {}", if n == 1 { "query" } else { "queries" }) };
                    ui.label(RichText::new(text).font(FontId::monospace(TXT_SMALL)).color(SUCCESS()));
                }
                if let Some(ms) = t.shell.ms {
                    ui.label(RichText::new(format!("{ms} ms")).font(FontId::monospace(TXT_SMALL)).color(if sql { MUTED() } else { SUCCESS() }))
                        .on_hover_text("From Run to the result, with starting the script and connecting");
                }
                if let Some(q) = t.shell.query_ms {
                    ui.label(RichText::new(format!("· query {q} ms")).font(FontId::monospace(TXT_SMALL)).color(MUTED()))
                        .on_hover_text("From the first connection being ready to the result");
                }
                let pages = t.shell.docs.len().div_ceil(PAGE_SIZE as usize);
                if has_table && pages > 1 {
                    ui.separator();
                    page_to = pager(ui, t.shell.page, pages);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    export = ui.button("Export…").clicked();
                    ui.scope(|ui| {
                        quiet_look(ui);
                        ui.menu_button("Copy as", |ui| {
                            menu_look(ui);
                            // None is JSON, Some the separator of a table. Rows are decoded only for the copy, and a result
                            // too big for the clipboard is left to Export.
                            let mut pick = None;
                            if ui.button("JSON").on_hover_text("The whole result").clicked() {
                                pick = Some(None);
                            }
                            if !t.shell.docs.is_empty() {
                                if ui.button("CSV").clicked() {
                                    pick = Some(Some(','));
                                }
                                if ui.button("TSV").clicked() {
                                    pick = Some(Some('\t'));
                                }
                            }
                            if let Some(sep) = pick {
                                let bytes = t.shell.docs_bytes();
                                if bytes > db::sql::export::CLIP_MAX {
                                    too_big = Some(bytes);
                                } else {
                                    ui.ctx().copy_text(match sep {
                                        None => t.shell.json_text(usize::MAX),
                                        Some(sep) => db::to_table(&t.shell.docs.iter().map(db::Row::owned).collect::<Vec<_>>(), sep),
                                    });
                                }
                                ui.close();
                            }
                        });
                    });
                    if !t.shell.docs.is_empty() {
                        let mut raw = t.shell.raw;
                        segmented(ui, &mut raw, &[(false, "Table"), (true, "JSON")]);
                        if raw != t.shell.raw {
                            acts.push(ShellA::Raw { tab: uid, on: raw });
                        }
                    }
                });
            });
            ui.add_space(4.0);
            if !has_table {
                // Very large results are cut for display; Export writes everything.
                const RAW_MAX: usize = 300_000;
                if t.shell.raw_text.is_none() {
                    let mut s = t.shell.json_text(RAW_MAX);
                    if s.len() > RAW_MAX {
                        let cut = (0..=RAW_MAX).rev().find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                        s.truncate(cut);
                        s.push_str("\n… cut for display. Use Export to save the whole result.");
                    }
                    t.shell.raw_text = Some(s);
                }
                let text = t.shell.raw_text.as_ref().unwrap();
                let raw_h = (ui.available_height() - SHELL_PANEL_H - 30.0).max(80.0);
                toolbar(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("shell-raw").max_height(raw_h).auto_shrink([false, true]).show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY).layouter(&mut code_layout));
                    });
                });
            }
        }
        if let Some(n) = too_big {
            self.warn(format!("The result is {} MB, too big to copy. Use Export to save it to a file.", n >> 20));
        }
        if export {
            if self.core.tabs[self.core.cur].shell.docs.is_empty() {
                self.export_shell();
            } else {
                let sel = self.checked_rows();
                self.open_export(sel, false);
            }
        }
        // Before the table below draws, so it shows the new page.
        if let Some(page) = page_to {
            self.exec(Action::Shell(ShellA::Page { tab: uid, page }));
        }
        if has_table {
            // The result table is the same one documents use: open, colours, right-click menu.
            // Sized to its rows so the Console sits right under a short result, not at the bottom.
            let rows = self.core.tabs[self.core.cur].shell.docs.len().min(PAGE_SIZE as usize) as f32;
            let h = (ui.available_height() - SHELL_PANEL_H - 34.0).max(80.0).min(TABLE_HEAD_H + rows * ROW_H + 10.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| self.results(ui, true));
        }
        // An advance adds item spacing even for an empty rect, which would shift the console.
        if result_ui.min_rect().height() > 0.0 {
            outer.advance_cursor_after_rect(result_ui.min_rect());
        }
        let ui = outer;
        ui.add_space(8.0);
        if let Some(on) = shell_panel(ui, &self.core.tabs[self.core.cur].shell, &conn_name, &kinds) {
            acts.push(ShellA::Queries { tab: uid, on });
        }
        if let Some(code) = use_example {
            acts.push(ShellA::Code { tab: uid, code });
        }
        self.core.new_pkg = new_pkg;
        if packages != self.core.prefs.shell_packages {
            acts.push(ShellA::Packages(packages));
            // Package changes need a fresh language server, which installs them so their types load.
            self.js_restart();
        }
        if saved {
            self.save_prefs();
        }
        if let (Some(n), Some((c, d, s))) = (sql_need, &sql_cx) {
            self.sql_serve(*c, d, s, Wants { schema: false, values: Some(n) });
        }
        if let Some(f) = editor {
            self.js_editor(ui.ctx(), f, &colls);
        }
        if stop {
            acts.push(ShellA::Stop { tab: uid });
        }
        if let Some(name) = detach.and_then(|i| self.core.tabs[self.core.cur].shell.attached.get(i)).map(|a| a.name.clone()) {
            acts.push(ShellA::Detach { tab: uid, name });
        }
        if !acts.is_empty() {
            ui.ctx().request_repaint();
        }
        for a in acts {
            self.exec(Action::Shell(a));
        }
        if open_attach {
            self.open_attach();
        }
        if run {
            self.run_shell_script();
        }
        if let Some(s) = strip.as_ref().filter(|_| make_default) {
            self.make_snippet_default(s);
        }
        if let Some(r) = make_value {
            self.ask_make_value(r, Src::Shell);
        }
        if let Some(b) = demo_pick {
            self.demo_open_picker(ui.ctx(), &b, "picker-shell");
        }
        // Last, since opening a script may change the tab this frame was drawn for.
        self.pick_acts(pick_acts);
    }

    /// QUARRY_TEST_SQL=shell: the SQL shell of the design, with a finished run and its Queries panel.
    pub(super) fn sql_shell_demo(&mut self) {
        let t = &mut self.core.tabs[0];
        t.view = View::Shell;
        t.selected = None;
        let ac_demo = std::env::var("QUARRY_TEST_SQL").is_ok_and(|v| v == "shellac");
        t.shell.code = if ac_demo { "async function run() {\n  const rows = await sql`\n    SELECT customer_id, COUNT(*) AS n FROM ord\n    WHERE status = ${'paid'} GROUP BY customer_id`\n  return rows\n}".into() } else { "async function run() {\n  const rows = await sql`\n    SELECT customer_id, COUNT(*) AS n FROM orders\n    WHERE status = ${'paid'} GROUP BY customer_id HAVING n > 3`\n  for (const r of rows) {\n    r.last = await orders.findOne({ customer_id: r.customer_id }, { orderBy: '-id' })\n  }\n  return rows\n}".into() };
        let rows = [(1284_i64, 9_i64, 4410_i64, "paid", "112.00"), (877, 7, 4409, "shipped", "26.75"), (61, 6, 4407, "paid", "1,204.10"), (430, 5, 4405, "paid", "54.20")];
        let docs: Vec<Bson> = rows.iter().map(|&(c, n, id, st, tot)| Bson::Document(doc! { "customer_id": c, "n": n, "last": { "id": id, "status": st, "total": tot } })).collect();
        t.shell.set_value(Bson::Array(docs));
        t.shell.hide_absent_id();
        t.shell.ms = Some(84);
        t.shell.queries = vec![
            db::ShellQuery { conn: "shop-prod".into(), text: "SELECT customer_id, COUNT(*) AS n FROM orders WHERE status = ? GROUP BY customer_id HAVING n > 3".into(), params: "[\"paid\"]".into(), rows: Some(12), affected: None, error: None, ms: 22 },
            db::ShellQuery { conn: "shop-prod".into(), text: "SELECT * FROM `orders` WHERE `customer_id` = ? ORDER BY `id` DESC LIMIT 1".into(), params: String::new(), rows: Some(1), affected: None, error: None, ms: 5 },
        ];
        for _ in 0..11 {
            let q = t.shell.queries[1].clone();
            t.shell.queries.push(q);
        }
        t.shell.queries_tab = true;
        if let Some(code) = shell_ls::shell_sig_code() {
            t.shell.code = code;
        }
        if matches!(std::env::var("QUARRY_TEST_SQL").as_deref(), Ok("shellattach" | "attach")) {
            self.attach_demo();
        }
    }

    /// Saves the whole Shell result: JSON for anything, CSV or TSV for document lists.
    pub(super) fn export_shell(&mut self) {
        let t = &self.core.tabs[self.core.cur].shell;
        let Some(v) = t.result() else { return };
        // Documents for CSV and TSV, decoded only for this save.
        let docs: Vec<Document> = t.docs.iter().map(db::Row::owned).collect();
        let mut dialog = rfd::FileDialog::new().set_file_name(format!("{}-shell.json", self.core.tabs[self.core.cur].sel.coll)).add_filter("JSON", &["json"]);
        if !docs.is_empty() {
            dialog = dialog.add_filter("CSV", &["csv"]).add_filter("TSV", &["tsv"]);
        }
        let Some(path) = dialog.save_file() else { return };
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("json").to_ascii_lowercase();
        let body = match ext.as_str() {
            "csv" if !docs.is_empty() => db::to_table(&docs, ','),
            "tsv" if !docs.is_empty() => db::to_table(&docs, '\t'),
            _ => serde_json::to_string_pretty(&v.into_relaxed_extjson()).unwrap_or_default(),
        };
        match std::fs::write(&path, body) {
            Ok(()) => self.ok(format!("Saved the shell result to {}", path.display())),
            Err(e) => self.err(format!("Could not save: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON view and Copy as JSON build their text a row at a time; it must be what the whole result gives.
    #[test]
    fn shell_json_text_matches_the_whole_result() {
        let docs = [doc! { "_id": 1, "a": { "b": [1, 2] }, "s": "x\ny" }, doc! {}, doc! { "n": Bson::Int64(5), "d": 2.5 }];
        let rows = || docs.iter().map(|d| db::Row::from_doc(d).unwrap()).collect::<Vec<_>>();
        let mut sh = ShellTab::default();
        sh.set_rows(rows(), false, true);
        assert!(sh.cut && sh.count == Some(3));
        assert_eq!(sh.json_text(usize::MAX), db::to_edit_value(&sh.result().unwrap()));
        // Stopped early: the start of the same text, cut for display by its caller.
        assert!(db::to_edit_value(&sh.result().unwrap()).starts_with(&sh.json_text(10)));
        sh.set_rows(rows()[..1].to_vec(), true, false);
        assert_eq!(sh.count, None);
        assert_eq!(sh.json_text(usize::MAX), db::to_edit_value(&Bson::Document(docs[0].clone())));
    }

    #[test]
    fn shell_scripts() {
        assert_eq!(query_script("orders", "{ a: 1 }", ""), "async function run() {\n\treturn await orders.find({ a: 1 })\n}");
        assert!(query_script("my.coll", "", "").contains("db.collection(\"my.coll\").find({})"));
        assert!(query_script("x", "db.x.find({});", "").ends_with("return await db.x.find({})\n}"));
        assert!(query_script("orders", "{ name: ~\"a.b\" }", "").contains(r#"$regex: "a\\.b", $options: "i""#));
        let mut t = Tab::new(Sel { coll: "o".into(), ..Default::default() });
        (t.filter_txt, t.sort_txt, t.limit_txt, t.skip_txt, t.hint_txt) = ("{}".into(), "{ a: -1 }".into(), "500".into(), "20".into(), "a_1".into());
        let chain = |t: &Tab| cursor_chain(&super::super::run::find_query(t).unwrap());
        assert_eq!(chain(&t), r#".sort({ a: -1 }).skip(20).limit(500).hint("a_1")"#);
        t.limit_txt = "00".into();
        assert_eq!(chain(&t), r#".sort({ a: -1 }).skip(20).hint("a_1")"#);
        t.proj_txt = "{ at: { $gt: #today } }".into();
        assert!(!chain(&t).contains('#'), "shortcuts are expanded");
        assert!(query_script("x", "db.x.aggregate([])", ".sort({ a: 1 })").ends_with("db.x.aggregate([])\n}"));
    }
}
