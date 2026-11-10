//! The JS shell view and running scripts.

use super::*;
use crate::ui::sql_complete::{Wants, analyze, split_statement};
use std::cell::RefCell;

/// Words the original treats as possibly destructive in the shell (`dangerousCommands`,
/// renderer line 131264: /drop|remove|replace|update|delete|insert|rename|\$out/i), plus `$merge`,
/// which writes to a collection from a pipeline just like `$out`.
pub(super) fn dangerous_words(code: &str, sql: bool) -> Vec<String> {
    let lower = code.to_lowercase();
    if sql {
        // Whole words only: SQL scripts name columns like created_at and updated_at all the time.
        const WORDS: [&str; 16] = ["drop", "replace", "update", "delete", "insert", "rename", "truncate", "alter", "create", "grant", "revoke", "merge", "call", "load", "lock", "kill"];
        let found: Vec<&str> = lower.split(|c: char| !(c.is_alphanumeric() || c == '_')).collect();
        return WORDS.iter().filter(|w| found.contains(w)).map(|w| w.to_string()).collect();
    }
    ["drop", "remove", "replace", "update", "delete", "insert", "rename", "$out", "$merge"].iter().filter(|w| lower.contains(*w)).map(|w| w.to_string()).collect()
}

/// The words to confirm before a script runs. A script that reaches both engines is checked for both, since
/// it cannot be told which handle a word belongs to.
pub(super) fn shell_ops(code: &str, main_sql: bool, attached_sql: impl Iterator<Item = bool>) -> Vec<String> {
    let (mut mongo, mut sql) = (!main_sql, main_sql);
    for s in attached_sql {
        (mongo, sql) = (mongo || !s, sql || s);
    }
    let mut out = if mongo { dangerous_words(code, false) } else { Vec::new() };
    if sql {
        for w in dangerous_words(code, true) {
            if !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out
}

/// The original's "Open query in NodeShell" script (renderer line 96080), with its collection
/// reference rule (getSnippetCollectionRef): names that are not plain words go through db.collection().
pub(super) fn query_script(coll: &str, filter: &str) -> String {
    let f = filter.trim().trim_end_matches(';');
    if db::is_shell(f) {
        return format!("async function run() {{\n\treturn await {f}\n}}");
    }
    let f = if f.is_empty() { "{}" } else { f };
    let expanded = db::expand_shortcuts(f);
    let f = expanded.as_deref().unwrap_or(f);
    format!("async function run() {{\n\treturn await {}.find({f})\n}}", collection_ref(coll))
}

/// Words the Shell runner does not bind as bare names, since they cannot be parameter names or
/// would not resolve to the collection inside run(). Must match RESERVED in shell_runner.mjs.
pub(super) const JS_RESERVED: [&str; 48] = [
    "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for",
    "function", "if", "import", "in", "instanceof", "new", "null", "return", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while",
    "with", "implements", "interface", "let", "package", "private", "protected", "public", "static", "yield", "await", "arguments", "eval",
];

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
const GUTTER_W: f32 = 52.0;
/// Height of the Console and Queries panel under the result, tab row included.
const SHELL_PANEL_H: f32 = 150.0;

/// The panel under the result: the script's console output, or every query it ran.
fn shell_panel(ui: &mut egui::Ui, sh: &mut ShellTab, conn: &str, kinds: &[(String, bool, Color32)]) {
    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 6 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(SHELL_PANEL_H - 12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let (n_log, n_q) = (sh.logs.len(), sh.queries.len());
            if view_tab(ui, &format!("Console  {n_log}"), !sh.queries_tab).clicked() {
                sh.queries_tab = false;
            }
            if view_tab(ui, &format!("Queries  {n_q}"), sh.queries_tab).clicked() {
                sh.queries_tab = true;
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
        ui.label(RichText::new("Values in ${} become ? placeholders and are never spliced into the text. Whole numbers beyond 2^53 come back as BigInt, and unsigned values above 9223372036854775807 come back as strings (send them back as a string or BigInt). An array becomes a list for IN (${ids}). Cmd+Enter runs, Stop interrupts the running statement.").color(MUTED()));
        if !writable {
            ui.label(RichText::new("This connection is read-only: only reading statements run, and every write is refused. Unlock writes in the top bar to allow them.").color(SUCCESS()));
        }
        caption(ui, "NAMES YOU CAN USE");
        egui::Grid::new("sql-shell-names").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (n, what) in [
                ("sql`...`", "run a statement; returns the rows as objects (writes return an empty list with .affected and .insertId)"),
                ("sql.unsafe(text, params)", "the same with text you built yourself and ? placeholders"),
                ("<table>", "every table of the database whose name is a plain word, e.g. orders.find()"),
                ("table('name')", "any table by its name, also names that are not plain words"),
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
        ui.label(RichText::new("find(where?, { select, orderBy, limit, offset })  ·  findOne  ·  count  ·  insert(row or rows)  ·  update(where, set)  ·  delete(where)").monospace().size(TXT_SMALL).color(TEXT()));
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
    /// Switches the current tab to the Shell with `code` ready to review and run.
    pub(super) fn open_in_shell(&mut self, code: String) {
        let Some(t) = self.tm() else { return };
        t.shell.code = code;
        t.view = View::Shell;
        t.selected = None;
    }

    pub(super) fn run_shell_script(&mut self) {
        let Some(t) = self.t() else { return };
        let code = t.shell.code.clone();
        let ops = shell_ops(&code, t.sql.is_some(), t.shell.attached.iter().filter_map(|a| self.conn(a.conn)).map(|c| c.meta.engine.is_sql()));
        // Same check as the original; on a read-only connection the runner refuses writes anyway.
        if !ops.is_empty() && self.shell_writable() {
            self.modal = Some(Modal::ShellWrite(ops));
        } else {
            self.start_shell();
        }
    }

    pub(super) fn start_shell(&mut self) {
        if self.demo {
            return self.ok("Offline demo: the shell needs a real connection.");
        }
        let Some(runtime) = find_bun(&self.prefs.shell_runtime) else {
            return self.err("Bun was not found. Install it from bun.sh, or set its path in Settings.");
        };
        let Some(t) = self.t() else { return };
        let (sel, code, main_sql) = (t.sel.clone(), t.shell.code.clone(), t.sql.is_some());
        let collections: Vec<String> = self.conn(sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == sel.db)).and_then(|d| d.1.clone()).unwrap_or_default();
        let packages: Vec<String> = self.prefs.shell_packages.iter().map(|p| db::package_spec(p).0).collect();
        let read_only = !self.sel_writable();
        let (mut mongo, mut sql_entries, mut sql) = match self.shell_attachments() {
            Ok(v) => v,
            Err(e) => {
                self.tabs[self.cur].shell.error = Some(e);
                return;
            }
        };
        // The tab's own connection is the main entry. A SQL script gets no address and no password: its queries come back to this app.
        let name = self.conn(sel.conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        if main_sql {
            let Some(db) = self.conn(sel.conn).and_then(|c| c.sql.clone()) else { return self.err("Connect first.") };
            sql_entries.insert(0, serde_json::json!({ "name": name, "db": sel.db, "tables": collections }));
            sql.insert(0, (name.clone(), db::SqlRelay { db, schema: sel.db.clone(), read_only, conn_id: Default::default() }));
        } else {
            let uri = match stored_uri(sel.conn) {
                Ok(u) => u,
                Err(e) => return self.err(uri_err(e)),
            };
            mongo.insert(0, serde_json::json!({ "name": name, "uri": uri, "db": sel.db, "proxy": self.shell_proxy(sel.conn), "collections": collections, "readOnly": read_only }));
        }
        let mut input = serde_json::json!({ "mongo": mongo, "sql": sql_entries });
        input["code"] = code.clone().into();
        input["packages"] = packages.into();
        input["timeoutMs"] = self.prefs.timeout_ms.into();
        input["main"] = name.into();
        let input = input.to_string();
        // The original's NodeShell.addHistory (renderer line 196411): skip scripts over 1000
        // characters, move a repeat to the top, keep MAX_HISTORY_LENGTH = 50 per database.
        const MAX_HISTORY_LENGTH: usize = 50;
        const MAX_HISTORY_SCRIPT: usize = 1000;
        if code.chars().count() <= MAX_HISTORY_SCRIPT {
            let h = self.prefs.shell_histories.entry(format!("{}/{}", sel.conn, sel.db)).or_default();
            h.retain(|x| *x != code);
            h.insert(0, code);
            h.truncate(MAX_HISTORY_LENGTH);
        }
        self.save_prefs();
        // A table added since the editor types were built shows up in the next ones.
        if main_sql {
            self.sql_columns_ask(true);
        }
        for a in self.tabs[self.cur].shell.attached.clone() {
            if let Some(db) = self.conn(a.conn).and_then(|c| c.sql.clone()) {
                self.sql_schema_asked.insert((a.conn, a.db.clone()));
                self.worker.sql_schema(a.conn, db, a.db);
            }
        }
        self.req += 1;
        let req = self.req;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sh = &mut self.tabs[self.cur].shell;
        *sh = ShellTab { code: std::mem::take(&mut sh.code), attached: std::mem::take(&mut sh.attached), req, cancel: Some(tx), started: Some(std::time::Instant::now()), queries_tab: sh.queries_tab, ..Default::default() };
        let packages = self.prefs.shell_packages.clone();
        self.tabs[self.cur].shell.relays = sql.iter().map(|(_, r)| (r.db.conn, r.conn_id.clone())).collect();
        self.worker.shell(req, db::ShellJob { runtime, dir: config_dir().join("shell"), input, packages, sql }, rx);
    }

    /// The Shell runs in its own process, so it reaches SSH-only servers through this app's
    /// tunnel (a local SOCKS5 port) or through the connection's own proxy.
    pub(super) fn shell_proxy(&self, conn: u64) -> serde_json::Value {
        let Some(c) = self.conn(conn) else { return serde_json::Value::Null };
        if let Some(t) = &c.tunnel {
            return serde_json::json!({ "host": "127.0.0.1", "port": t.port });
        }
        match &c.meta.proxy {
            Some(p) => serde_json::json!({ "host": p.host, "port": if p.port == 0 { 1080 } else { p.port }, "user": p.user, "password": secret(conn, "proxy-password") }),
            None => serde_json::Value::Null,
        }
    }

    pub(super) fn shell_view(&mut self, ui: &mut egui::Ui) {
        let writable = self.sel_writable();
        let sql = self.t().is_some_and(|t| t.sql.is_some());
        let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        let (conn_name, db_name) = self.t().and_then(|t| self.conn(t.sel.conn).map(|c| (c.meta.name.clone(), t.sel.db.clone()))).unwrap_or_default();
        let own_pill = self.own_pill(&conn_name, &db_name, !writable);
        let att_pills = self.attached_pills();
        let kinds = self.shell_kinds();
        let (mut detach, mut open_attach) = (None, false);
        if sql {
            self.sql_columns_ask(false);
        }
        let history = self.t().and_then(|t| self.prefs.shell_histories.get(&format!("{}/{}", t.sel.conn, t.sel.db)).cloned()).unwrap_or_default();
        let mut packages = self.prefs.shell_packages.clone();
        let mut pkg_names: Vec<String> = packages.iter().map(|p| package_ident(p)).collect();
        if packages.iter().any(|p| db::package_spec(p).0 == "lodash") {
            pkg_names.push("_".into());
        }
        let mut new_pkg = std::mem::take(&mut self.new_pkg);
        let colls: Vec<String> = self.t().and_then(|t| self.conn(t.sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == t.sel.db)).and_then(|d| d.1.clone())).unwrap_or_default();
        let known = if sql { Vec::new() } else { self.db_fields() };
        // What SQL templates in the script complete from: the tab's database and its looked up values.
        let sql_cx = sql.then(|| (self.tabs[self.cur].sel.conn, self.tabs[self.cur].sel.db.clone()));
        let scope = sql_cx.as_ref().map(|(c, d)| Self::sql_scope(&self.conns, &self.sql_schema, &self.views, *c, d, None));
        let none = Fields::new();
        let values = sql_cx.as_ref().and_then(|k| self.sql_values.get(k)).unwrap_or(&none);
        let (t, ac) = (&mut self.tabs[self.cur], &mut self.ac);
        if t.shell.code.is_empty() {
            t.shell.code = default_script(&coll, sql);
        }
        let running = t.shell.started;
        let (mut run, mut stop, mut editor) = (false, false, None);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_enabled_ui(running.is_none(), |ui| {
                run = run_button(ui, "Run", "⌘↵").on_hover_text("Cmd+Enter").clicked();
            });
            run |= running.is_none() && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
            stop = quiet_enabled(ui, running.is_some(), "Stop").on_hover_text("End the script and interrupt its running statement").clicked();
            if let Some(at) = running {
                spin(ui);
                ui.label(RichText::new(format!("Running {:.1} s", at.elapsed().as_secs_f32())).color(MUTED()));
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), Sense::hover());
            ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, LINE()));
            ui.label(RichText::new("Connections").color(MUTED()));
            // The pills share what the Attach button and the Packages, History and Help buttons on the right leave.
            let text_w = |t: &str| ui.painter().layout_no_wrap(t.to_string(), FontId::proportional(TXT_BODY), TEXT()).size().x;
            let pad = ui.spacing().button_padding.x * 2.0 + ui.spacing().item_spacing.x;
            let right = text_w("Help") + text_w("History") + text_w(&format!("Packages {}", packages.len())) + 3.0 * pad + 110.0;
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
                if ui.selectable_label(t.shell.help, "Help").on_hover_text("What the shell can do, with examples").clicked() {
                    t.shell.help = !t.shell.help;
                }
                ui.menu_button("History", |ui| {
                    menu_look(ui);
                    ui.set_max_width(520.0);
                    if ui.button("Reset to the starter script").clicked() {
                        t.shell.code = default_script(&coll, sql);
                        ui.close();
                    }
                    ui.separator();
                    if history.is_empty() {
                        ui.label(RichText::new("Nothing run yet").color(MUTED()));
                    }
                    for h in &history {
                        let first: String = h.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("async function")).take(1).collect();
                        if ui.button(RichText::new(first.chars().take(80).collect::<String>()).monospace()).on_hover_text(h.as_str()).clicked() {
                            t.shell.code = h.clone();
                            ui.close();
                        }
                    }
                });
                sticky_menu(ui, format!("Packages {}", packages.len()), |ui| {
                    ui.set_min_width(300.0);
                    ui.label(RichText::new("npm packages for your scripts. Each one is loaded for you and ready by the name shown.").size(TXT_SMALL).color(MUTED()));
                    let mut drop = None;
                    for (i, p) in packages.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(p).monospace().color(BRIGHT()));
                            let alias = if db::package_spec(p).0 == "lodash" { format!("{}, _", package_ident(p)) } else { package_ident(p) };
                            ui.label(RichText::new(format!("ready as {alias}")).monospace().size(TXT_SMALL).color(C_DATE()));
                            if ui.button("🗙").on_hover_text("Remove").clicked() {
                                drop = Some(i);
                            }
                        });
                    }
                    if let Some(i) = drop {
                        packages.remove(i);
                    }
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut new_pkg).hint_text("lodash  or  dayjs@1.11").desired_width(190.0).font(egui::TextStyle::Monospace));
                        let valid = db::valid_package(new_pkg.trim());
                        let name = db::package_spec(new_pkg.trim()).0;
                        let mut add = ui.add_enabled(valid, egui::Button::new("Add"));
                        if db::PINNED_PACKAGES.contains(&name.as_str()) {
                            add = add.on_disabled_hover_text(format!("{name} is built in, at the version the shell needs"));
                        }
                        if (add.clicked() || (valid && r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))) && !packages.contains(&new_pkg.trim().to_string()) {
                            packages.push(new_pkg.trim().to_string());
                            new_pkg.clear();
                        }
                    });
                    ui.label(RichText::new("Installed by Bun on the next run. Bun does not run install scripts of packages unless you trust them.").size(TXT_CAPTION).color(FAINT()));
                });
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
        // The editor: line numbers, token colors, and the same frame for both engines.
        let id = ui.make_persistent_id("shell-editor");
        let focused = ui.memory(|m| m.has_focus(id));
        // Read before the list consumes it, so a late server answer cannot reopen a dismissed list.
        let esc = ui.input(|i| i.key_pressed(Key::Escape));
        // The editor keeps three lines, and the results below it the Console, the header and three rows.
        let (lo, hi) = (3.0 * LINE_H + 24.0, ui.available_height() - SHELL_PANEL_H - 3.0 * ROW_H - TABLE_HEAD_H - 70.0);
        let pref = self.prefs.shell_editor_h;
        let custom = pref > 0.0;
        let h = if custom { pref.clamp(lo, hi.max(lo)) } else { (t.shell.code.split('\n').count() as f32 * LINE_H + 24.0).clamp(110.0, (ui.available_height() * 0.4).max(110.0)) };
        let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let mut job = if sql { shell_ls::js_sql_job(buf.as_str()) } else { js_job(buf.as_str()) };
            for sec in &mut job.sections {
                sec.format.line_height = Some(LINE_H);
            }
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        // Inside sql`...` the suggestions are SQL ones; elsewhere the built-in and server ones stay.
        let tail = if scope.is_some() && focused { shell_ls::template_tail(ui.ctx(), id, &t.shell.code) } else { String::new() };
        let (meta, need): (RefCell<Option<(String, String)>>, RefCell<Option<crate::ui::sql_complete::Need>>) = Default::default();
        let compute = |b: &str| -> Option<(usize, Vec<Sug>)> {
            let tpl = scope.as_ref().and_then(|_| sql_at(b, b.len()));
            let (Some(tpl), Some(scope)) = (tpl, &scope) else { return suggest_js(b, &known, &colls, &pkg_names) };
            let (sb, full) = split_statement(&sql_text(b, &tpl, b.len()).0, &tail);
            let o = analyze(&sb, &full, scope, values)?;
            *need.borrow_mut() = o.need.clone();
            *meta.borrow_mut() = Some((o.title.clone(), o.foot.clone()));
            (!o.sugs.is_empty()).then(|| (sb[o.from..].chars().count(), o.sugs))
        };
        if let (Some(scope), true) = (&scope, std::env::var("QUARRY_TEST_SQL").is_ok_and(|v| v == "shellac")) {
            shell_ls::shell_ac_demo(ui, id, &t.shell.code, ac, scope, values);
        }
        shell_ls::shell_sig_demo(ui, id, &t.shell.code, ac, &mut self.js);
        let mut sql_need = None;
        let framed = egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("shell-ed").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                let (out, picked) = ac_field(ui, id, &mut t.shell.code, ac, &compute, |ui, text, _open| {
                    egui::TextEdit::multiline(text)
                        .id(id)
                        .font(FontId::monospace(TXT_SMALL))
                        .frame(egui::Frame::new().inner_margin(egui::Margin { left: GUTTER_W as i8, right: 12, top: 10, bottom: 10 }))
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, if custom { h } else { 96.0 }))
                        .lock_focus(true)
                        .layouter(&mut layout)
                        .show(ui)
                });
                if ac.id == Some(id) && !ac.items.is_empty() {
                    if let Some((title, foot)) = meta.borrow_mut().take() {
                        (ac.title, ac.foot) = (title, foot);
                    }
                }
                sql_need = need.borrow_mut().take();
                let caret = out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0);
                let (g, gp, rect) = (&out.galley, out.galley_pos, out.response.rect);
                let cur_line = caret.map_or(0, |c| t.shell.code.chars().take(c).filter(|ch| *ch == '\n').count() + 1);
                let (mut n, mut fresh) = (0, true);
                for row in &g.rows {
                    if fresh {
                        n += 1;
                        let y = gp.y + row.pos.y + row.height() / 2.0;
                        ui.painter().text(egui::pos2(rect.left() + GUTTER_W - 14.0, y), egui::Align2::RIGHT_CENTER, n.to_string(), FontId::monospace(TXT_CAPTION), if n == cur_line { MUTED() } else { FAINT() });
                    }
                    fresh = row.ends_with_newline;
                }
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
        });
        let bar = splitter(ui);
        let saved = resize_editor(&bar, &mut self.prefs.shell_editor_h, framed.response.rect.height(), lo, hi);
        ui.add_space(2.0);
        if let Some(e) = &t.shell.error {
            egui::Frame::new().fill(tint(ALERT(), 20)).corner_radius(R_MD).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(e).monospace().color(ALERT()));
            });
            ui.add_space(8.0);
        }
        let has_table = !t.shell.docs.is_empty() && !t.shell.raw;
        let (mut export, mut page_to) = (false, None);
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
                if !t.shell.queries.is_empty() {
                    let mut conns: Vec<&str> = t.shell.queries.iter().map(|q| q.conn.as_str()).collect();
                    conns.sort_unstable();
                    conns.dedup();
                    let n = t.shell.queries.len();
                    let text = if conns.len() > 1 { format!("{n} queries on {} connections", conns.len()) } else { format!("{n} {}", if n == 1 { "query" } else { "queries" }) };
                    ui.label(RichText::new(text).font(FontId::monospace(TXT_SMALL)).color(SUCCESS()));
                }
                if let Some(ms) = t.shell.ms {
                    ui.label(RichText::new(format!("{ms} ms")).font(FontId::monospace(TXT_SMALL)).color(if sql { MUTED() } else { SUCCESS() }));
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
                        ui.menu_button("Copy as ▾", |ui| {
                            menu_look(ui);
                            let docs: Vec<Document> = t.shell.docs.iter().map(db::Row::owned).collect();
                            let mut text = None;
                            if ui.button("JSON").on_hover_text("The whole result").clicked() {
                                text = Some(t.shell.result().map(|v| db::to_edit_value(&v)).unwrap_or_default());
                            }
                            if !docs.is_empty() {
                                if ui.button("CSV").clicked() {
                                    text = Some(db::to_table(&docs, ','));
                                }
                                if ui.button("TSV").clicked() {
                                    text = Some(db::to_table(&docs, '\t'));
                                }
                            }
                            if let Some(s) = text {
                                ui.ctx().copy_text(s);
                                ui.close();
                            }
                        });
                    });
                    if !t.shell.docs.is_empty() {
                        segmented(ui, &mut t.shell.raw, &[(false, "Table"), (true, "JSON")]);
                    }
                });
            });
            ui.add_space(4.0);
            if !has_table {
                // Very large results are cut for display; Export writes everything.
                const RAW_MAX: usize = 300_000;
                if t.shell.raw_text.is_none() {
                    let mut s = t.shell.result().map(|v| db::to_edit_value(&v)).unwrap_or_default();
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
        if let Some(p) = page_to {
            t.shell.page = p;
            // Checks are positions in the page on screen.
            t.shell_checked.clear();
        }
        if export {
            if self.tabs[self.cur].shell.docs.is_empty() {
                self.export_shell();
            } else {
                let sel = self.checked_rows();
                self.open_export(sel, false);
            }
        }
        if page_to.is_some() {
            self.drop_agg_row();
        }
        if has_table {
            // The result table is the same one documents use: open, colours, right-click menu.
            // Sized to its rows so the Console sits right under a short result, not at the bottom.
            let rows = self.tabs[self.cur].shell.docs.len().min(PAGE_SIZE as usize) as f32;
            let h = (ui.available_height() - SHELL_PANEL_H - 34.0).max(80.0).min(TABLE_HEAD_H + rows * ROW_H + 10.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| self.results(ui, true));
        }
        ui.add_space(8.0);
        shell_panel(ui, &mut self.tabs[self.cur].shell, &conn_name, &kinds);
        if let Some(code) = use_example {
            self.tabs[self.cur].shell.code = code;
        }
        self.new_pkg = new_pkg;
        if packages != self.prefs.shell_packages {
            self.prefs.shell_packages = packages;
            self.save_prefs();
            self.js_restart();
        }
        if saved {
            self.save_prefs();
        }
        if let (Some(n), Some((c, d))) = (sql_need, &sql_cx) {
            self.sql_serve(*c, d, Wants { schema: false, values: Some(n) });
        }
        if let Some(f) = editor {
            self.js_editor(ui.ctx(), f, &colls);
        }
        if stop {
            if let Some(c) = self.tabs[self.cur].shell.cancel.take() {
                let _ = c.send(());
            }
        }
        if let Some(i) = detach {
            self.tabs[self.cur].shell.attached.remove(i);
        }
        if open_attach {
            self.open_attach();
        }
        if run {
            self.run_shell_script();
        }
    }

    /// QUARRY_TEST_SQL=shell: the SQL shell of the design, with a finished run and its Queries panel.
    pub(super) fn sql_shell_demo(&mut self) {
        let t = &mut self.tabs[0];
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
        let t = &self.tabs[self.cur].shell;
        let Some(v) = t.result() else { return };
        // Documents for CSV and TSV, decoded only for this save.
        let docs: Vec<Document> = t.docs.iter().map(db::Row::owned).collect();
        let mut dialog = rfd::FileDialog::new().set_file_name(format!("{}-shell.json", self.tabs[self.cur].sel.coll)).add_filter("JSON", &["json"]);
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

    #[test]
    fn shell_scripts() {
        assert_eq!(query_script("orders", "{ a: 1 }"), "async function run() {\n\treturn await orders.find({ a: 1 })\n}");
        assert!(query_script("my.coll", "").contains("db.collection(\"my.coll\").find({})"));
        assert!(query_script("x", "db.x.find({});").ends_with("return await db.x.find({})\n}"));
        assert!(query_script("orders", "{ name: ~\"a.b\" }").contains(r#"$regex: "a\\.b", $options: "i""#));
        assert_eq!(dangerous_words("db.x.deleteMany({})", false), ["delete"]);
        assert_eq!(dangerous_words("db.x.aggregate([{ $merge: { into: 'y' } }])", false), ["$merge"]);
        assert_eq!(dangerous_words("await orders.delete({ id: 1 }); sql`TRUNCATE t`", true), ["delete", "truncate"]);
        assert_eq!(dangerous_words("sql`CALL p(); LOCK TABLES t READ; KILL 5; LOAD DATA INFILE 'x' INTO TABLE t`", true), ["call", "load", "lock", "kill"]);
        assert!(dangerous_words("sql`SELECT created_at, updated_at FROM orders`", true).is_empty());
        // A script with both engines is checked for both, each word once.
        assert_eq!(shell_ops("x.deleteMany({}); sql`DELETE FROM t`", false, [true].into_iter()), ["delete"]);
        assert_eq!(shell_ops("db.x.aggregate([{ $out: 'y' }]); sql`TRUNCATE t`", true, [false].into_iter()), ["$out", "truncate"]);
        assert!(shell_ops("sql`SELECT 1`", true, std::iter::empty()).is_empty());
    }
}
