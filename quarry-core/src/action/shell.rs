//! The Shell (src/app/shell.rs draws it): running scripts with Bun on the tab's connection and the ones attached to it,
//! their answers, and the choices of its toolbar and result. The language server stays in the app with its popups.

use super::tools::find_bun;
use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::Core;
use crate::db;
use crate::library::snippet::Lang;
use crate::paths::config_dir;
use crate::state::config::{secret, stored_uri, uri_err};
use crate::state::library::snippet_strip::{fill_tab, Src};
use crate::state::shell_attach::{Attached, PG_SCHEMA};
use crate::state::{ns_key, ShellTab, View};
use mongodb::bson::Document;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ShellA {
    /// Run the script of the current tab, which has this uid: confirmed first when it may write.
    Run { tab: u64 },
    /// Run it without the confirm, after it or for a script already filled in.
    Start { tab: u64 },
    Stop { tab: u64 },
    /// Put this script in the editor: History, Reset to the starter script, an example's Use.
    Code { tab: u64, code: String },
    /// Detach the connection attached under this name.
    Detach { tab: u64, name: String },
    /// Attach the database `db` of `conn` as `name` (a PostgreSQL one with its `schema`).
    Attach { tab: u64, conn: u64, db: String, name: String, allow_writes: bool, schema: String },
    Help { tab: u64, on: bool },
    /// The panel under the result: Queries (true) or Console.
    Queries { tab: u64, on: bool },
    /// The result as JSON (true) or a table.
    Raw { tab: u64, on: bool },
    Page { tab: u64, page: usize },
    /// The snippet strip's Filled (true) or Template.
    Filled { tab: u64, on: bool },
    /// The npm packages every script gets.
    Packages(Vec<String>),
    /// The editor's text after a frame of typing. A script the Cmd+K form filled reads its braces as placeholders again.
    Edit { tab: u64, code: String },
    /// The Attach dialog asks for the schemas of a PostgreSQL database; the answer fills the dialog.
    PgSchemas { conn: u64, db: String },
    /// Opens a Shell tab on the current tab's database with this script in it.
    OpenIn { code: String },
}

/// Words the original treats as possibly destructive in the shell (`dangerousCommands`,
/// renderer line 131264: /drop|remove|replace|update|delete|insert|rename|\$out/i), plus `$merge`,
/// which writes to a collection from a pipeline just like `$out`.
pub fn dangerous_words(code: &str, sql: bool) -> Vec<String> {
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
pub fn shell_ops(code: &str, main_sql: bool, attached_sql: impl Iterator<Item = bool>) -> Vec<String> {
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

impl Core {
    pub(super) fn shell(&mut self, a: ShellA) -> Result<Reply, Refusal> {
        let tab = match &a {
            ShellA::Packages(p) => {
                self.prefs.shell_packages = p.clone();
                self.save_prefs();
                return Ok(Reply::Done);
            }
            ShellA::OpenIn { code } => {
                self.open_in_shell(code.clone());
                return Ok(Reply::Done);
            }
            ShellA::PgSchemas { conn, db } => {
                if let Some(sql) = self.conn(*conn).and_then(|c| c.sql.clone()) {
                    self.worker.pg_shell_list(*conn, sql, db.clone(), String::new());
                }
                return Ok(Reply::Done);
            }
            ShellA::Edit { tab, .. } | ShellA::Run { tab } | ShellA::Start { tab } | ShellA::Stop { tab } | ShellA::Code { tab, .. } | ShellA::Detach { tab, .. } | ShellA::Attach { tab, .. } | ShellA::Help { tab, .. } | ShellA::Queries { tab, .. } | ShellA::Raw { tab, .. } | ShellA::Page { tab, .. } | ShellA::Filled { tab, .. } => *tab,
        };
        let Some(i) = self.tabs.iter().position(|t| t.uid == tab) else {
            // A write confirm left open while its tab closed must not run another tab's script.
            return if matches!(a, ShellA::Run { .. } | ShellA::Start { .. }) { Err(self.refuse(super::tabs::TAB_GONE)) } else { Ok(Reply::Done) };
        };
        let t = &mut self.tabs[i];
        match a {
            // A run reads the current tab throughout, as the editor, the library and the confirm call it there.
            ShellA::Run { .. } | ShellA::Start { .. } if i != self.cur => return Err(self.refuse("That Shell tab is no longer the current one.")),
            ShellA::Run { .. } => return self.run_shell_script(),
            ShellA::Start { .. } => return self.start_shell(),
            ShellA::Stop { .. } => t.shell.stop(),
            ShellA::Code { code, .. } => t.shell.code = code,
            ShellA::Edit { code, .. } => {
                t.shell.code = code;
                t.snip.off = false;
            }
            ShellA::Detach { name, .. } => t.shell.attached.retain(|a| a.name != name),
            ShellA::Attach { conn, db, name, allow_writes, schema, .. } => {
                let pg = self.conn(conn).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres);
                self.tabs[i].shell.attached.push(Attached { conn, db: db.clone(), name, allow_writes, schema: if pg { schema } else { String::new() }, tables: Vec::new() });
                self.attach_load(conn, &db);
            }
            ShellA::Help { on, .. } => t.shell.help = on,
            ShellA::Queries { on, .. } => t.shell.queries_tab = on,
            ShellA::Raw { on, .. } => t.shell.raw = on,
            ShellA::Page { page, .. } => {
                t.shell.page = page;
                // Checks are positions in the page on screen.
                t.shell_checked.clear();
                // The open rows of the result table were rows of the page left.
                t.expanded.retain(|k| !k.0);
                if t.selected.is_some_and(|s| s.0) {
                    t.selected = None;
                    (self.json_edit, self.inline, self.new_field) = (None, None, None);
                }
            }
            ShellA::Filled { on, .. } => t.snip.filled = on,
            ShellA::Packages(_) | ShellA::PgSchemas { .. } | ShellA::OpenIn { .. } => {}
        }
        Ok(Reply::Done)
    }

    /// Switches the current tab to the Shell with `code` ready to review and run.
    pub fn open_in_shell(&mut self, code: String) {
        let Some(t) = self.tm() else { return };
        t.shell.code = code;
        t.view = View::Shell;
        t.selected = None;
    }

    /// The script that runs: the editor's text with its `{{...}}` values filled in, or why it can't be.
    pub fn shell_code(&self) -> Result<String, String> {
        let Some(t) = self.t() else { return Ok(String::new()) };
        fill_tab(t, &t.shell.code, Src::Shell, Lang::Shell, true)
    }

    fn run_shell_script(&mut self) -> Result<Reply, Refusal> {
        // The confirm judges what will run, and a value that does not read stops the run before it.
        let code = match self.shell_code() {
            Ok(c) => c,
            Err(e) => return Err(self.refuse(e)),
        };
        let Some(t) = self.t() else { return Ok(Reply::Done) };
        let ops = shell_ops(&code, t.sql.is_some(), t.shell.attached.iter().filter_map(|a| self.conn(a.conn)).map(|c| c.meta.engine.is_sql()));
        // Same check as the original; on a read-only connection the runner refuses writes anyway.
        if !ops.is_empty() && self.shell_writable() {
            let tab = t.uid;
            self.ui_fx.push(UiFx::Open(Dialog::ShellWrite { tab, ops }));
            Ok(Reply::Done)
        } else {
            self.start_shell()
        }
    }

    fn start_shell(&mut self) -> Result<Reply, Refusal> {
        if self.demo {
            self.ok("Offline demo: the shell needs a real connection.");
            return Ok(Reply::Done);
        }
        let Some(runtime) = find_bun(&self.prefs.shell_runtime) else {
            return Err(self.refuse_settings("Bun is needed for the Shell. Install it from bun.sh, or set its path in Settings."));
        };
        let Some(t) = self.t() else { return Ok(Reply::Done) };
        let (sel, code, main_sql) = (t.sel.clone(), t.shell.code.clone(), t.sql.is_some());
        // Every run passes here: the editor, the library and the write confirm. History keeps the text as written.
        let filled = match self.shell_code() {
            Ok(c) => c,
            Err(e) => return Err(self.refuse(e)),
        };
        let collections: Vec<String> = self.tables_of(&sel);
        let schema = self.sel_schema(&sel);
        let packages: Vec<String> = self.prefs.shell_packages.iter().map(|p| db::package_spec(p).0).collect();
        let read_only = !self.sel_writable();
        // A PostgreSQL attachment's tables are not saved with the tab, so a restored one lists them again. They arrive
        // while the script starts, and a table helper missing on this run is there on the next (table('name') always works).
        let unlisted: Vec<(u64, String)> = self.t().map(|t| t.shell.attached.iter().filter(|a| a.tables.is_empty() && self.conn(a.conn).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres)).map(|a| (a.conn, a.db.clone())).collect()).unwrap_or_default();
        for (c, d) in unlisted {
            self.attach_load(c, &d);
        }
        let (mut mongo, mut sql_entries, mut sql) = match self.shell_attachments() {
            Ok(v) => v,
            Err(e) => {
                let cur = self.cur;
                self.tabs[cur].shell.error = Some(e);
                return Ok(Reply::Done);
            }
        };
        // The tab's own connection is the main entry. A SQL script gets no address and no password: its queries come back to this app.
        let name = self.conn(sel.conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        if main_sql {
            let Some(db) = self.conn(sel.conn).and_then(|c| c.sql.clone()) else { return Err(self.refuse_offline(sel.conn)) };
            if db.dialect == db::sql::Dialect::Postgres {
                // The tab's database picks the pool, and the script starts in the tab's schema.
                sql_entries.insert(0, serde_json::json!({ "name": name, "db": sel.db, "tables": collections, "dialect": "postgres" }));
                sql.insert(0, (name.clone(), db::SqlRelay { db: db.at(&sel.db), schema, read_only, conn_id: Default::default() }));
            } else {
                sql_entries.insert(0, serde_json::json!({ "name": name, "db": sel.db, "tables": collections, "dialect": "mysql" }));
                sql.insert(0, (name.clone(), db::SqlRelay { db, schema: sel.db.clone(), read_only, conn_id: Default::default() }));
            }
        } else {
            // Without a live client there is no SSH tunnel either, and the stored address would then reach whatever answers locally.
            if self.conn(sel.conn).is_none_or(|c| c.client.is_none()) {
                return Err(self.refuse_offline(sel.conn));
            }
            let uri = match stored_uri(sel.conn) {
                Ok(u) => u,
                Err(e) => {
                    self.err(uri_err(e));
                    return Ok(Reply::Done);
                }
            };
            mongo.insert(0, serde_json::json!({ "name": name, "uri": uri, "db": sel.db, "proxy": self.shell_proxy(sel.conn), "collections": collections, "readOnly": read_only }));
        }
        let mut input = serde_json::json!({ "mongo": mongo, "sql": sql_entries });
        input["code"] = filled.into();
        input["packages"] = packages.into();
        input["timeoutMs"] = self.prefs.timeout_ms.into();
        input["maxBytes"] = db::SHELL_MAX.into();
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
                // A PostgreSQL schema is read on a session of its own database and cached under that database and schema.
                let (key, space, db) = if db.dialect == db::sql::Dialect::Postgres {
                    let s = pg_schema(&a.schema).to_string();
                    (ns_key(&a.db, &s), s, db.at(&a.db))
                } else {
                    (a.db.clone(), a.db.clone(), db)
                };
                self.sql_schema_asked.insert((a.conn, key));
                self.worker.sql_schema(a.conn, db, space);
            }
        }
        self.req += 1;
        let req = self.req;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cur = self.cur;
        // The floating selection bar is not disabled with the result, so stale checks could still be exported.
        self.tabs[cur].shell_checked.clear();
        let sh = &mut self.tabs[cur].shell;
        // Only the script and the last result are kept, so a field added to ShellTab later starts fresh.
        let old = std::mem::take(sh);
        *sh = ShellTab {
            code: old.code,
            attached: old.attached,
            agent_code: old.agent_code,
            queries_tab: old.queries_tab,
            value: old.value,
            docs: old.docs,
            fields: old.fields,
            page: old.page,
            raw: old.raw,
            raw_text: old.raw_text,
            count: old.count,
            cut: old.cut,
            ms: old.ms,
            query_ms: old.query_ms,
            req,
            cancel: Some(tx),
            started: Some(std::time::Instant::now()),
            ..Default::default()
        };
        let packages = self.prefs.shell_packages.clone();
        self.tabs[cur].shell.relays = sql.iter().map(|(_, r)| (r.db.conn, r.conn_id.clone())).collect();
        self.worker.shell(req, db::ShellJob { runtime, dir: config_dir().join("shell"), input, packages, sql }, rx);
        Ok(Reply::Pending(req))
    }

    /// The Shell runs in its own process, so it reaches SSH-only servers through this app's
    /// tunnel (a local SOCKS5 port) or through the connection's own proxy.
    pub fn shell_proxy(&self, conn: u64) -> serde_json::Value {
        let Some(c) = self.conn(conn) else { return serde_json::Value::Null };
        match &c.tunnel {
            Some(db::Way::Ssh(t)) => return serde_json::json!({ "host": "127.0.0.1", "port": t.port }),
            // The Cloud SQL proxy is plain TCP and only serves PostgreSQL, which the shell reaches through the app, not SOCKS.
            Some(db::Way::CloudSql(_)) => return serde_json::Value::Null,
            // A SQL connection's own proxy is dialed by the app, and the shell reaches SQL through the app's relay.
            Some(db::Way::Proxy(_)) => return serde_json::Value::Null,
            None => {}
        }
        match &c.meta.proxy {
            Some(p) => serde_json::json!({ "host": p.host, "port": if p.port == 0 { 1080 } else { p.port }, "user": p.user, "password": secret(conn, "proxy-password") }),
            None => serde_json::Value::Null,
        }
    }

    /// Asks for what an attached database needs on the editor side: the table list and schema of a SQL
    /// one. Collections of a Mongo connection arrive when it connects. Demo mode has no server.
    pub fn attach_load(&mut self, conn: u64, db: &str) {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        if self.demo {
            return;
        }
        if sql.dialect == db::sql::Dialect::Postgres {
            // The schema is the attachment's own, so each PostgreSQL attachment on `db` asks for its tables. Every tab, since
            // an agent may fill one in the background.
            let mut schemas: Vec<String> = self.tabs.iter().flat_map(|t| &t.shell.attached).filter(|a| a.conn == conn && a.db == db).map(|a| pg_schema(&a.schema).to_string()).collect();
            schemas.sort();
            schemas.dedup();
            for s in schemas {
                // The columns type the attachment's tables in the editor, as the schema of a MySQL attachment does.
                self.ask_sql_schema(conn, sql.at(db), ns_key(db, &s), s.clone());
                self.worker.pg_shell_list(conn, sql.clone(), db.to_string(), s);
            }
            return;
        }
        if self.sql_asked.insert((conn, db.to_string())) {
            self.worker.sql_tables(conn, sql.clone(), db.to_string());
        }
        self.ask_sql_schema(conn, sql, db.to_string(), db.to_string());
    }

    /// Whether any target of the current tab's script can be written to: its own connection, or an
    /// attachment that is allowed to write and whose connection is unlocked.
    pub fn shell_writable(&self) -> bool {
        self.sel_writable() || self.t().is_some_and(|t| t.shell.attached.iter().any(|a| a.allow_writes && self.conn(a.conn).is_some_and(|c| c.writable)))
    }

    /// The attachments as the runner and the relay take them, or what stops the run.
    fn shell_attachments(&self) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>, Vec<(String, db::SqlRelay)>), String> {
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
                    relays.push((a.name.clone(), db::SqlRelay { db: db.at(&a.db), schema: pg_schema(&a.schema).into(), read_only, conn_id: Default::default() }));
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

    /// The key of the current tab's database in the column cache.
    pub fn shell_sql_key(&self) -> (u64, String) {
        let t = &self.tabs[self.cur];
        (t.sel.conn, ns_key(&t.sel.db, &self.sel_schema(&t.sel)))
    }

    /// Asks for the schema of the current SQL tab's database, which autocomplete shares. `again`
    /// refreshes one already held, as a run does so that a table added since shows up.
    pub fn sql_columns_ask(&mut self, again: bool) {
        let key = self.shell_sql_key();
        if self.demo || (self.sql_schema_asked.contains(&key) && !again) {
            return;
        }
        let Some((db, space)) = self.sql_target(&self.tabs[self.cur].sel) else { return };
        self.sql_schema_asked.insert(key.clone());
        self.worker.sql_schema(key.0, db, space);
    }

    /// The tables part of a `pg_shell_list` answer: they go to the attachments of every tab that match, so a script run
    /// after this has its table helpers. The schemas part fills the app's Attach dialog.
    pub fn shell_pg_tables(&mut self, d: &Document) {
        let (conn, database, schema) = (d.get_i64("conn").unwrap_or_default() as u64, d.get_str("database").unwrap_or_default(), d.get_str("schema").unwrap_or_default());
        if schema.is_empty() || d.contains_key("error") {
            return;
        }
        let tables: Vec<String> = d.get_array("tables").map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        for t in &mut self.tabs {
            for a in t.shell.attached.iter_mut().filter(|a| a.conn == conn && a.db == database && (a.schema == schema || a.schema.is_empty() && schema == PG_SCHEMA)) {
                a.tables = tables.clone();
            }
        }
    }

    /// The SQL Shell asks once for the tables and columns its script editor completes from.
    pub(crate) fn shell_schema_tick(&mut self) {
        if self.t().is_some_and(|t| t.view == View::Shell && t.sql.is_some()) {
            self.sql_columns_ask(false);
        }
    }

    /// The JSON text the Shell shows for a result that is not a table, built once per result and cut for display.
    pub(crate) fn fill_shell_raw(&mut self) {
        const RAW_MAX: usize = 300_000;
        let Some(t) = self.tabs.get_mut(self.cur).filter(|t| t.view == View::Shell && t.shell.raw_text.is_none()) else { return };
        if t.shell.value.is_none() && t.shell.docs.is_empty() || !t.shell.docs.is_empty() && !t.shell.raw {
            return;
        }
        let mut s = t.shell.json_text(RAW_MAX);
        if s.len() > RAW_MAX {
            let cut = (0..=RAW_MAX).rev().find(|i| s.is_char_boundary(*i)).unwrap_or(0);
            s.truncate(cut);
            s.push_str("\n… cut for display. Use Export to save the whole result.");
        }
        t.shell.raw_text = Some(s);
    }

    pub(crate) fn shell_log(&mut self, req: u64, level: String, text: String) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) {
            t.shell.push_log(level, text);
        }
    }

    pub(crate) fn shell_query(&mut self, req: u64, mut q: db::ShellQuery) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) {
            // The panel keeps the latest queries only, a loop can run millions.
            if q.text.len() > 2000 {
                q.text = q.text.chars().take(2000).collect();
            }
            t.shell.queries.push(q);
            // Trimmed in batches so a loop does not shift a thousand entries per query.
            if t.shell.queries.len() > 1500 {
                let over = t.shell.queries.len() - 1000;
                t.shell.queries.drain(..over);
            }
        }
    }

    pub(crate) fn shell_done(&mut self, req: u64, res: db::Res<db::ShellOut<mongodb::bson::Bson>>, ms: u128, query_ms: Option<u64>) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) else { return };
        t.rows_replaced();
        let sql = t.sql.is_some();
        let sh = &mut t.shell;
        sh.cancel = None;
        sh.started = None;
        (sh.ms, sh.query_ms) = (Some(ms), query_ms);
        match res {
            Ok(out) => {
                match out {
                    db::ShellOut::Value(v) => sh.set_value(v),
                    db::ShellOut::Docs { rows, single, cut } => sh.set_rows(rows, single, cut),
                }
                if sql {
                    sh.hide_absent_id();
                }
            }
            Err(e) => {
                sh.value = None;
                sh.docs.clear();
                (sh.fields, sh.count, sh.cut) = (Vec::new(), None, false);
                sh.error = Some(e);
            }
        }
    }
}

/// The schema of an attachment, `public` when it names none.
pub fn pg_schema(schema: &str) -> &str {
    if schema.is_empty() { PG_SCHEMA } else { schema }
}
