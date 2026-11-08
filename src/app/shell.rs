//! The JS shell view and running scripts.

use super::*;

/// Words the original treats as possibly destructive in the shell (`dangerousCommands`,
/// renderer line 131264: /drop|remove|replace|update|delete|insert|rename|\$out/i), plus `$merge`,
/// which writes to a collection from a pipeline just like `$out`.
pub(super) fn dangerous_words(code: &str) -> Vec<String> {
    let lower = code.to_lowercase();
    ["drop", "remove", "replace", "update", "delete", "insert", "rename", "$out", "$merge"].iter().filter(|w| lower.contains(*w)).map(|w| w.to_string()).collect()
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
pub(super) fn shell_help(ui: &mut egui::Ui, coll: &str, writable: bool) -> Option<String> {
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
    ui.menu_button("🗐 Copy as code", |ui| {
        menu_look(ui);
        for l in db::Lang::ALL {
            if ui.button(l.name()).clicked() {
                *pick = Some(l);
                ui.close();
            }
        }
    });
}

pub(super) fn default_script(coll: &str) -> String {
    let target = collection_ref(coll);
    format!("async function run() {{\n  return await {target}.find({{}}).limit(20).toArray();\n}}")
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
        let ops = dangerous_words(&code);
        // Same check as the original; on a read-only connection the runner refuses writes anyway.
        if !ops.is_empty() && self.sel_writable() {
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
        let sel = t.sel.clone();
        let code = t.shell.code.clone();
        let uri = match stored_uri(sel.conn) {
            Ok(u) => u,
            Err(e) => return self.err(uri_err(e)),
        };
        let collections: Vec<String> = self.conn(sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == sel.db)).and_then(|d| d.1.clone()).unwrap_or_default();
        let input = serde_json::json!({
            "uri": uri, "db": sel.db, "code": code, "collections": collections,
            "readOnly": !self.sel_writable(), "timeoutMs": self.prefs.timeout_ms,
            "packages": self.prefs.shell_packages.iter().map(|p| db::package_spec(p).0).collect::<Vec<_>>(),
            "proxy": self.shell_proxy(sel.conn),
        })
        .to_string();
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
        self.req += 1;
        let req = self.req;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sh = &mut self.tabs[self.cur].shell;
        *sh = ShellTab { code: std::mem::take(&mut sh.code), req, cancel: Some(tx), started: Some(std::time::Instant::now()), ..Default::default() };
        let packages = self.prefs.shell_packages.clone();
        self.worker.shell(req, db::ShellJob { runtime, dir: config_dir().join("shell"), input, packages }, rx);
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
        let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        let history = self.t().and_then(|t| self.prefs.shell_histories.get(&format!("{}/{}", t.sel.conn, t.sel.db)).cloned()).unwrap_or_default();
        let mut packages = self.prefs.shell_packages.clone();
        let mut pkg_names: Vec<String> = packages.iter().map(|p| package_ident(p)).collect();
        if packages.iter().any(|p| db::package_spec(p).0 == "lodash") {
            pkg_names.push("_".into());
        }
        let mut new_pkg = std::mem::take(&mut self.new_pkg);
        let colls: Vec<String> = self.t().and_then(|t| self.conn(t.sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == t.sel.db)).and_then(|d| d.1.clone())).unwrap_or_default();
        let (t, ac) = (&mut self.tabs[self.cur], &mut self.ac);
        let paths = t.paths.clone();
        if t.shell.code.is_empty() {
            t.shell.code = default_script(&coll);
        }
        let running = t.shell.started;
        let mut run = false;
        let mut stop = false;
        let mut editor = None;
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, "SHELL  ·  JavaScript run by Bun with the official MongoDB driver");
                ui.label(RichText::new("?").color(PRIMARY())).on_hover_text(
                    "Write a function and return a value; a cursor is cut to 100 documents.\nAvailable: db, every collection by name, ObjectId, ISODate, console.log, randomId, UUIDv4.",
                );
                if !writable {
                    ui.label(RichText::new("read-only: writes are refused").size(TXT_CAPTION).color(SUCCESS()));
                }
            });
            let id = ui.make_persistent_id("shell-editor");
            // Read before the list consumes it, so a late server answer cannot reopen a dismissed list.
            let esc = ui.input(|i| i.key_pressed(Key::Escape));
            let (out, picked) = ac_field(ui, id, &mut t.shell.code, ac, &|b| suggest_js(b, &paths, &colls, &pkg_names), |ui, text, _open| {
                egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(10).lock_focus(true).layouter(&mut js_layout).show(ui)
            });
            let caret = out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0);
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
            ui.horizontal(|ui| {
                match running {
                    Some(at) => {
                        spin(ui);
                        ui.label(RichText::new(format!("Running {:.1} s", at.elapsed().as_secs_f32())).color(MUTED()));
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                        stop = ui.button("Stop").clicked();
                    }
                    None => {
                        run = primary(ui, "Run", PRIMARY()).on_hover_text("Cmd+Enter").clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                        if let Some(ms) = t.shell.ms {
                            let n = t.shell.count.map(|n| format!("{n} results  ")).unwrap_or_default();
                            ui.label(RichText::new(format!("{n}{ms} ms")).color(MUTED()));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("History", |ui| {
                        menu_look(ui);
                        ui.set_max_width(520.0);
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
                    sticky_menu(ui, format!("Packages ({})", packages.len()), |ui| {
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
                    if ui.selectable_label(t.shell.help, "Help").on_hover_text("What the shell can do, with examples").clicked() {
                        t.shell.help = !t.shell.help;
                    }
                    if ui.button("Reset").on_hover_text("Back to the starter script").clicked() {
                        t.shell.code = default_script(&coll);
                    }
                });
            });
        });
        ui.add_space(6.0);
        let mut use_example = None;
        let has_table = !t.shell.docs.is_empty() && !t.shell.raw;
        let top_h = if has_table { (ui.available_height() * 0.3).max(60.0) } else { f32::INFINITY };
        egui::ScrollArea::vertical().id_salt("shell-out").max_height(top_h).auto_shrink([false, true]).show(ui, |ui| {
            if t.shell.help {
                use_example = shell_help(ui, &collection_ref(&coll), writable);
                ui.add_space(6.0);
            }
            if !t.shell.logs.is_empty() {
                toolbar(ui, |ui| {
                    ui.set_width(ui.available_width());
                    caption(ui, "LOG");
                    for (level, text) in &t.shell.logs {
                        let c = match level.as_str() {
                            "error" => ALERT(),
                            "warn" => WARNING(),
                            "info" => PRIMARY(),
                            _ => TEXT(),
                        };
                        ui.label(RichText::new(text).monospace().size(TXT_BODY).color(c));
                    }
                });
                ui.add_space(6.0);
            }
            if let Some(e) = &t.shell.error {
                egui::Frame::new().fill(tint(ALERT(), 20)).corner_radius(R_MD).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(e).monospace().color(ALERT()));
                });
            }
        });
        let mut export = false;
        let mut page_to = None;
        if t.shell.value.is_some() || !t.shell.docs.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                caption(ui, "RESULT");
                let what = match (t.shell.docs.len(), t.shell.count) {
                    (0, Some(n)) => format!("{n} items"),
                    (0, None) => t.shell.value.as_ref().map_or("", db::type_name).to_string(),
                    (n, _) => format!("{} documents", group(&n.to_string())),
                };
                ui.label(RichText::new(what).color(BRIGHT()));
                let pages = t.shell.docs.len().div_ceil(PAGE_SIZE as usize);
                if has_table && pages > 1 {
                    ui.separator();
                    page_to = pager(ui, t.shell.page, pages);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    export = ui.button("📤 Export…").clicked();
                    if ui.button("Copy").on_hover_text("Copy the whole result as JSON").clicked() {
                        ui.ctx().copy_text(t.shell.result().map(|v| db::to_edit_value(&v)).unwrap_or_default());
                    }
                    if !t.shell.docs.is_empty() {
                        ui.selectable_value(&mut t.shell.raw, true, "JSON");
                        ui.selectable_value(&mut t.shell.raw, false, "Table");
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
                toolbar(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("shell-raw").auto_shrink(false).show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY).layouter(&mut code_layout));
                    });
                });
            }
        }
        if let Some(p) = page_to {
            t.shell.page = p;
        }
        if export {
            self.export_shell();
        }
        if page_to.is_some() {
            self.drop_agg_row();
        }
        if has_table {
            // The result table is the same one documents use: open, colours, right-click menu.
            self.results(ui, true);
        }
        if let Some(code) = use_example {
            self.tabs[self.cur].shell.code = code;
        }
        self.new_pkg = new_pkg;
        if packages != self.prefs.shell_packages {
            self.prefs.shell_packages = packages;
            self.save_prefs();
            self.js_restart();
        }
        if let Some(f) = editor {
            self.js_editor(ui.ctx(), f, &colls);
        }
        if stop {
            if let Some(c) = self.tabs[self.cur].shell.cancel.take() {
                let _ = c.send(());
            }
        }
        if run {
            self.run_shell_script();
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
        assert_eq!(dangerous_words("db.x.deleteMany({})"), ["delete"]);
        assert_eq!(dangerous_words("db.x.aggregate([{ $merge: { into: 'y' } }])"), ["$merge"]);
    }
}
