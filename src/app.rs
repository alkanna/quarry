use crate::ui::{cells::*, complete::*, editor::*, theme::*, widgets::*};
use crate::db::{self, Msg, PAGE_SIZE, Query, ShellKind, Target, UndoAction, UndoKind, Worker};
use eframe::egui::{self, Color32, FontId, Key, RichText, Sense, Stroke, text::LayoutJob};
use mongodb::Client;
use mongodb::bson::{Bson, Document, doc, oid::ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod aggregate;
mod chrome;
mod compare;
mod config;
mod demo;
mod documents;
mod import;
mod indexes;
mod log;
mod modals;
mod poll;
mod schema;
mod shell;
mod sidebar;
mod state;
mod tools;
mod tree;
mod users;
use config::*;
use documents::*;
use shell::*;
use sidebar::*;
use state::*;
use tools::*;
use tree::*;

const SYSTEM_DBS: [&str; 3] = ["admin", "config", "local"];
/// Columns shown before the user picks their own; drawing every field of a wide collection is slow.
const DEFAULT_COLS: usize = 8;

/// Shortcuts listed by Cmd+/. Cmd is Ctrl on Linux and Windows.
const SHORTCUTS: [(&str, &str); 22] = [
    ("Cmd+K, Cmd+T", "Jump to a connection, database or collection"),
    ("Cmd+R", "Reload the current tab"),
    ("Cmd+Shift+R", "Refresh the connection's databases"),
    ("Cmd+W", "Close the tab"),
    ("Cmd+Shift+T", "Reopen the last closed tab"),
    ("Cmd+1 to Cmd+8", "Go to that tab"),
    ("Cmd+9", "Go to the last tab"),
    ("Ctrl+Tab, Cmd+Shift+]", "Next tab"),
    ("Ctrl+Shift+Tab, Cmd+Shift+[", "Previous tab"),
    ("Cmd+Alt+1 to Cmd+Alt+7", "Documents, Aggregate, Indexes, Schema, Import, Compare, Shell"),
    ("Cmd+F", "Focus the query"),
    ("Cmd+Shift+F", "Find a collection in the sidebar"),
    ("Up, Down", "Select the previous or next document"),
    ("PageUp, PageDown", "Previous or next page"),
    ("Enter", "Run the query"),
    ("Cmd+E", "Explain the query or pipeline"),
    ("Cmd+Enter", "Run the pipeline or script"),
    ("Esc", "Close or deselect"),
    ("Cmd+,", "Settings"),
    ("Cmd+N", "New connection"),
    ("Cmd+L", "Show or hide the query log"),
    ("Cmd+/", "This list"),
];

/// Relations as the original's export file (exportRelations, renderer line 135294), so files
/// move between Quarry and the original.
fn relations_export(db: &str, list: &[Relation]) -> String {
    let rel: Vec<serde_json::Value> = list.iter().map(|r| serde_json::json!({ "collection": r.coll, "path": r.path, "referencedCollection": r.target, "previewField": r.preview })).collect();
    let now = mongodb::bson::DateTime::now().try_to_rfc3339_string().unwrap_or_default();
    serde_json::to_string_pretty(&serde_json::json!({ "version": "1.0", "database": db, "exportedAt": now, "relations": rel })).unwrap_or_default()
}

/// Reads an export file; files without `version` and `relations` are refused, like the original
/// (importRelations, 135332). Array markers in the original's paths ("items[].id") are dropped.
fn relations_import(text: &str) -> db::Res<Vec<Relation>> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let (Some(_), Some(list)) = (v.get("version"), v.get("relations").and_then(|r| r.as_array())) else {
        return Err("Invalid data relations file format".into());
    };
    let s = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    Ok(list.iter().map(|r| Relation { coll: s(r, "collection"), path: s(r, "path").replace("[]", ""), target: s(r, "referencedCollection"), preview: s(r, "previewField") }).filter(|r| !r.coll.is_empty() && !r.path.is_empty() && !r.target.is_empty()).collect())
}

pub struct App {
    worker: Worker,
    rx: Receiver<Msg>,
    conns: Vec<Conn>,
    active: Option<u64>,
    open_dbs: HashSet<(u64, String)>,
    tabs: Vec<Tab>,
    cur: usize,
    req: u64,
    json_edit: Option<JsonEdit>,
    drafts: HashMap<String, Draft>,
    saving: HashMap<String, Draft>,
    /// Paths changed in the document being drawn, for tinting.
    changed: HashSet<String>,
    inline: Option<Inline>,
    new_field: Option<NewField>,
    /// Pending expand/collapse from the tree menu: document, path prefix, open.
    fold: Option<(egui::Id, Vec<String>, bool)>,
    fold_busy: bool,
    /// Key column width and full row span of the tree being drawn, so values line up and hover bands cover the whole card.
    key_w: f32,
    char_w: f32,
    tree_x: egui::Rangef,
    insert: Option<String>,
    /// Database hidden from the sidebar while its drop runs, to show again if the drop fails.
    dropping_db: Option<(u64, String)>,
    prefs: UiPrefs,
    new_col: String,
    col_filter: String,
    #[cfg(target_os = "macos")]
    menu: Option<crate::menu::MenuBar>,
    /// Menu picks that are not key presses, handled at the start of the next frame.
    #[cfg(target_os = "macos")]
    menu_acts: Vec<crate::menu::Act>,
    new_pkg: String,
    new_lib: String,
    /// Views per (connection, database), shown differently in the sidebar.
    views: HashSet<(u64, String, String)>,
    users: Option<UsersWin>,
    conn_test: Option<db::Res<String>>,
    idx_keys: String,
    idx_unique: bool,
    /// Usage and suggestions of the collection "db.coll" the Indexes view last asked about.
    idx_info: Option<(String, db::IndexInfo)>,
    undo: Vec<UndoAction>,
    explain_req: u64,
    modal: Option<Modal>,
    status: (String, bool),
    status_at: std::time::Instant,
    log: LogUi,
    /// Message, when it was shown, and the `at` of the undo entry its Undo button reverts.
    toast: Option<(String, std::time::Instant, Option<std::time::Instant>)>,
    ac: Ac,
    tree_filter: String,
    tools_installing: bool,
    /// Tree row the palette jumped to, as (connection, database, collection), scrolled into view once drawn.
    reveal: Option<(u64, String, Option<String>)>,
    /// Scroll the selected document row into view on its next draw, after keyboard selection.
    scroll_sel: bool,
    demo: bool,
    tool: Option<ToolRun>,
    /// CPU time of the last frame in milliseconds, for the frame rate readout.
    frame_ms: f32,
    /// Measured column widths per result set and column.
    width_cache: HashMap<(usize, usize, String, bool), f32>,
    /// Recently closed tabs, newest first.
    closed: Vec<Tab>,
    /// Version installed by the updater, waiting for a restart.
    updated: Option<String>,
    update_note: String,
    update_checked: Option<std::time::Instant>,
    last_input: std::time::Instant,
    started: std::time::Instant,
    shot: Option<(PathBuf, u32, Arc<Mutex<Option<egui::ColorImage>>>)>,
}

fn net_of(meta: &ConnMeta) -> db::Net {
    db::Net {
        ssh: meta.ssh.clone().map(|c| (c, crate::ssh::SshSecrets { password: secret(meta.id, "ssh-password"), passphrase: secret(meta.id, "ssh-passphrase") })),
        proxy: meta.proxy.clone().map(|p| db::Proxy { password: secret(meta.id, "proxy-password"), ..p }),
    }
}

/// Top-level fields of the shown documents, `_id` first, in the order they appear.
fn auto_fields(docs: &[Document]) -> Vec<String> {
    let mut cols = vec!["_id".to_string()];
    let mut seen: HashSet<&str> = HashSet::from(["_id"]);
    for d in docs {
        for k in d.keys() {
            if seen.insert(k) {
                cols.push(k.clone());
            }
        }
    }
    cols
}

pub(crate) fn collection_ref(coll: &str) -> String {
    if coll.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') && !coll.starts_with(|c: char| c.is_ascii_digit()) && !JS_RESERVED.contains(&coll) {
        coll.to_string()
    } else {
        format!("db.collection({})", serde_json::Value::String(coll.into()))
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>) -> Self {
        style(&cc.egui_ctx);
        let (tx, rx) = channel();
        // QUARRY_DEMO=1 opens the offline demo to click around in, without screenshot mode closing it.
        let demo = shot.is_some() || std::env::var("QUARRY_DEMO").is_ok();
        let mut app = Self {
            worker: Worker::new(tx, cc.egui_ctx.clone()),
            rx,
            conns: Vec::new(),
            active: None,
            open_dbs: HashSet::new(),
            tabs: Vec::new(),
            cur: 0,
            req: 0,
            json_edit: None,
            drafts: HashMap::new(),
            saving: HashMap::new(),
            changed: HashSet::new(),
            inline: None,
            new_field: None,
            fold: None,
            fold_busy: false,
            key_w: 0.0,
            char_w: 0.0,
            tree_x: egui::Rangef::NOTHING,
            insert: None,
            dropping_db: None,
            prefs: UiPrefs::default(),
            new_col: String::new(),
            col_filter: String::new(),
            // Screenshot runs stay off the real menu bar.
            #[cfg(target_os = "macos")]
            menu: (!demo || std::env::var("QUARRY_DEMO").is_ok()).then(|| crate::menu::MenuBar::new(&cc.egui_ctx)),
            #[cfg(target_os = "macos")]
            menu_acts: Vec::new(),
            new_pkg: String::new(),
            new_lib: String::new(),
            views: HashSet::new(),
            users: None,
            conn_test: None,
            idx_keys: String::new(),
            idx_unique: false,
            idx_info: None,
            undo: Vec::new(),
            explain_req: 0,
            modal: None,
            status: ("Ready. All connections start read-only.".into(), false),
            toast: None,
            ac: Ac::default(),
            status_at: std::time::Instant::now(),
            log: LogUi::default(),
            tree_filter: String::new(),
            tools_installing: false,
            reveal: None,
            scroll_sel: false,
            demo,
            tool: None,
            width_cache: HashMap::new(),
            frame_ms: 0.0,
            closed: Vec::new(),
            updated: None,
            update_note: String::new(),
            update_checked: None,
            last_input: std::time::Instant::now(),
            started: std::time::Instant::now(),
            shot: shot.map(|p| (p, 0, Arc::new(Mutex::new(None)))),
        };
        // Test fixture: QUARRY_TEST_JSON=<file of Extended JSON documents> makes screenshot mode
        // show those documents through the same message path real query results take.
        let fixture = std::env::var("QUARRY_TEST_JSON").ok().filter(|_| demo);
        if let Some(path) = fixture {
            app.demo = false;
            let docs: Vec<Document> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| db::parse_value(&t).ok())
                .and_then(|b| match b {
                    Bson::Array(a) => Some(a.into_iter().filter_map(|d| d.as_document().cloned()).collect()),
                    _ => None,
                })
                .unwrap_or_default();
            let sel = Sel { conn: 1, db: "fixture".into(), coll: "documents".into() };
            app.conns.push(Conn { meta: ConnMeta { id: 1, name: "Fixture".into(), ..Default::default() }, client: None, status: Status::Ready, writable: false, dbs: vec![("fixture".into(), Some(vec!["documents".into()]))], masked_uri: String::new() , tunnel: None });
            app.active = Some(1);
            app.open_dbs.insert((1, "fixture".into()));
            app.open_collection(sel, false);
            app.req += 1;
            app.tabs[0].req = app.req;
            let n = docs.len() as u64;
            app.worker.send(Msg::Docs { req: app.req, res: Ok((docs, n, 3)) });
        } else if demo {
            app.load_demo();
            // Test switches: the demo picture in the light theme, and with the find bar open.
            if std::env::var("QUARRY_TEST_LIGHT").is_ok() {
                Appearance::Light.apply(&cc.egui_ctx);
            }
            if let Ok(q) = std::env::var("QUARRY_TEST_FIND") {
                let f = Find { open: true, query: q, ..Default::default() };
                cc.egui_ctx.data_mut(|d| d.insert_temp(egui::Id::unique("json-editor-text").with("find"), f));
            }
        } else {
            crate::update::cleanup();
            let (metas, broken_conns) = parse_config_list::<ConnMeta>(&config_dir().join("connections.json"));
            let ui_json = config_dir().join("ui.json").exists().then_some(());
            let (prefs, broken_prefs) = parse_config(&config_dir().join("ui.json"));
            app.prefs = prefs;
            app.prefs.appearance.apply(&cc.egui_ctx);
            for meta in metas {
                let masked = keychain(meta.id).and_then(|k| k.get_password().ok()).map(|u| db::mask_uri(&u)).unwrap_or_default();
                app.conns.push(Conn { meta, client: None, status: Status::Idle, writable: false, dbs: Vec::new(), masked_uri: masked , tunnel: None });
            }
            app.active = app.conns.first().map(|c| c.meta.id);
            // Reopen the tabs of the last session; they load once their connection is connected.
            for st in app.prefs.open_tabs.clone() {
                if app.conn(st.conn).is_none() {
                    continue;
                }
                let mut t = Tab::new(Sel { conn: st.conn, db: st.db, coll: st.coll });
                t.view = match st.view.as_str() {
                    "Aggregate" => View::Aggregate,
                    "Indexes" => View::Indexes,
                    "Shell" => View::Shell,
                    "Schema" => View::Schema,
                    "Import" => View::Import,
                    "Compare" => View::Compare,
                    _ => View::Documents,
                };
                (t.filter_txt, t.sort_txt, t.proj_txt, t.shell.code) = (st.filter, st.sort, st.project, st.shell);
                if !st.agg.is_empty() {
                    t.agg_txt = st.agg;
                }
                t.agg_text_mode = st.agg_text_mode;
                let stages: Vec<Stage> = st.stages.iter().map(|s| Stage::new(&s.op, &s.text, s.on)).collect();
                if !stages.is_empty() && text_from_stages(&stages) == t.agg_txt {
                    t.stages_src = t.agg_txt.clone();
                    t.stages = stages;
                }
                app.tabs.push(t);
            }
            app.cur = app.prefs.cur_tab.min(app.tabs.len().saturating_sub(1));
            if let Some(t) = app.tabs.get(app.cur) {
                app.active = Some(t.sel.conn);
            }
            let current = env!("CARGO_PKG_VERSION");
            if let Some(id) = app.prefs.last_conn.filter(|&id| app.conn(id).is_some()) {
                app.active = Some(id);
            }
            if let Some(id) = app.active {
                app.connect(id);
            }
            if app.prefs.last_version != current {
                // A fresh install has no ui.json, so it skips the notes. Settings from 0.2.0 and
                // earlier lack last_version, so updating from those shows them.
                if ui_json.is_some()
                    && let Some(body) = crate::update::changelog(current)
                {
                    app.modal = Some(Modal::Text { title: format!("What's new in Quarry {current}"), body, code: false });
                }
                app.prefs.last_version = current.into();
                app.save_prefs();
            }
            let broken: Vec<String> = [broken_conns, broken_prefs].into_iter().flatten().collect();
            if !broken.is_empty() {
                app.modal = Some(Modal::Text { title: "Some settings could not be loaded".into(), body: broken.join("\n\n"), code: false });
            }
        }
        app
    }

    fn write_config(&mut self, file: &str, bytes: Vec<u8>) {
        if self.demo {
            return;
        }
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        // Written beside the file, then renamed over it: a crash mid-write leaves the old file
        // whole instead of a truncated one that no longer parses.
        let tmp = dir.join(format!(".{file}.tmp"));
        if let Err(err) = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, dir.join(file))) {
            let _ = std::fs::remove_file(&tmp);
            self.err(format!("Could not save {file}: {err}"));
        }
    }

    fn save_conns(&mut self) {
        let metas: Vec<&ConnMeta> = self.conns.iter().map(|c| &c.meta).collect();
        let b = serde_json::to_vec_pretty(&metas).unwrap_or_default();
        self.write_config("connections.json", b);
    }

    fn save_prefs(&mut self) {
        let b = serde_json::to_vec_pretty(&self.prefs).unwrap_or_default();
        self.write_config("ui.json", b);
    }

    fn ok(&mut self, s: impl Into<String>) {
        self.status = (s.into(), false);
        self.status_at = std::time::Instant::now();
    }

    fn err(&mut self, s: impl Into<String>) {
        self.status = (s.into(), true);
        self.status_at = std::time::Instant::now();
    }

    fn conn(&self, id: u64) -> Option<&Conn> {
        self.conns.iter().find(|c| c.meta.id == id)
    }

    fn t(&self) -> Option<&Tab> {
        self.tabs.get(self.cur)
    }

    fn tm(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.cur)
    }

    fn sel_is_view(&self) -> bool {
        self.t().is_some_and(|t| self.views.contains(&(t.sel.conn, t.sel.db.clone(), t.sel.coll.clone())))
    }

    fn sel_writable(&self) -> bool {
        if self.sel_is_view() {
            return false;
        }
        self.t().and_then(|t| self.conn(t.sel.conn)).is_some_and(|c| c.writable)
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.prefs.timeout_ms)
    }

    fn target_for(&self, s: &Sel) -> Option<Target> {
        let client = self.conn(s.conn)?.client.clone()?;
        Some(Target { client, db: s.db.clone(), coll: s.coll.clone(), timeout: self.timeout() })
    }

    fn target(&self) -> Option<Target> {
        self.target_for(&self.t()?.sel)
    }

    /// The single gate every write goes through.
    fn writable_target(&mut self) -> Option<(u64, Target)> {
        let conn = self.t()?.sel.conn;
        if !self.sel_writable() {
            self.err("This connection is read-only. Unlock writes in the top bar first.");
            return None;
        }
        match self.target() {
            Some(t) => Some((conn, t)),
            None => {
                self.err("Not connected.");
                None
            }
        }
    }

    fn ask_unlock(&mut self) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, None));
        }
    }

    /// Asks to allow writes, then runs `then` if they were allowed.
    fn ask_unlock_then(&mut self, then: impl FnOnce(&mut App) + 'static) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, Some(Box::new(then))));
        }
    }

    fn connect(&mut self, id: u64) {
        // Demo ids overlap real ones, so reading the Keychain here would connect to a real server.
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let uri = match stored_uri(id) {
            Ok(u) => u,
            Err(e) => return self.err(uri_err(e)),
        };
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        c.status = Status::Connecting;
        c.client = None;
        c.tunnel = None;
        let net = net_of(&c.meta);
        self.worker.connect(id, uri, self.timeout(), net);
    }

    fn col_key(&self, agg: bool) -> String {
        let s = self.t().map(|t| format!("{}/{}/{}", t.sel.conn, t.sel.db, t.sel.coll)).unwrap_or_default();
        match (agg, self.t().map(|t| t.view)) {
            (true, Some(View::Shell)) => format!("{s}#shell"),
            (true, Some(View::Import)) => format!("{s}#import"),
            (true, _) => format!("{s}#agg"),
            _ => s,
        }
    }

    fn reset_doc_state(&mut self) {
        self.json_edit = None;
        self.inline = None;
        self.new_field = None;
    }

    /// Opens a collection. Like the original (treeOpenInNewTab), a new tab is used unless one
    /// already shows that collection; `replace` reuses the current tab instead.
    fn open_collection(&mut self, sel: Sel, replace: bool) {
        self.reset_doc_state();
        if let Some(i) = self.tabs.iter().position(|t| t.sel == sel) {
            self.cur = i;
            return;
        }
        if replace && !self.tabs.is_empty() {
            let view = self.tabs[self.cur].view;
            self.tabs[self.cur] = Tab { view, ..Tab::new(sel) };
        } else {
            self.tabs.push(Tab::new(sel));
            self.cur = self.tabs.len() - 1;
        }
        self.reload();
    }

    /// Keeps the 10 most recently closed tabs for "Reopen closed tab" (maxClosedTabsCount,
    /// renderer line 197320).
    fn remember_closed(&mut self, t: Tab) {
        self.closed.insert(0, t);
        self.closed.truncate(10);
    }

    fn reopen_tab(&mut self) {
        if self.closed.is_empty() {
            return;
        }
        let t = self.closed.remove(0);
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
        self.reset_doc_state();
        self.reload();
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(i);
        self.remember_closed(t);
        if self.cur > i || self.cur >= self.tabs.len() {
            self.cur = self.cur.saturating_sub(1);
        }
        self.reset_doc_state();
    }

    fn run_query(&mut self) {
        let Some(t) = self.t() else { return };
        if db::is_shell(&t.filter_txt) {
            let text = t.filter_txt.clone();
            return self.run_shell(&text);
        }
        let parse = |s: &str| -> db::Res<Option<Document>> { if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some) } };
        let q = (|| -> db::Res<Query> {
            Ok(Query {
                filter: db::parse_doc(&t.filter_txt).map_err(|e| format!("Filter: {e}"))?,
                sort: parse(&t.sort_txt).map_err(|e| format!("Sort: {e}"))?,
                projection: parse(&t.proj_txt).map_err(|e| format!("Projection: {e}"))?,
                ..Default::default()
            })
        })();
        match q {
            Ok(q) => {
                let (f, so, p) = (t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone());
                let key = self.col_key(false);
                let h = self.prefs.history.entry(key).or_default();
                push_history(&mut h.selector, &f);
                push_history(&mut h.sort, &so);
                push_history(&mut h.project, &p);
                self.save_prefs();
                let t = self.tm().unwrap();
                t.query = q;
                t.page = 0;
                self.reload();
            }
            Err(e) => self.err(e),
        }
    }

    /// Opens the current query or pipeline as a snippet for one language.
    fn show_code(&mut self, lang: db::Lang, find: bool) {
        let Some(t) = self.t() else { return };
        let from_shell = |s: &str| db::parse_shell(s).map(|sh| match sh.kind {
            db::ShellKind::Find(q) => db::CodeOf::Find(q),
            db::ShellKind::Aggregate(p) => db::CodeOf::Aggregate(p),
        });
        let opt = |s: &str, what: &str| if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some).map_err(|e| format!("{what}: {e}")) };
        let what = if find && !db::is_shell(&t.filter_txt) {
            (|| Ok(db::CodeOf::Find(Query { filter: db::parse_doc(&t.filter_txt).map_err(|e| format!("Filter: {e}"))?, sort: opt(&t.sort_txt, "Sort")?, projection: opt(&t.proj_txt, "Projection")?, ..Default::default() })))()
        } else if find {
            from_shell(&t.filter_txt)
        } else if db::is_shell(&t.agg_txt) {
            from_shell(&t.agg_txt)
        } else {
            db::parse_pipeline(&t.agg_txt).map(db::CodeOf::Aggregate)
        };
        match what {
            Ok(w) => self.modal = Some(Modal::Text { title: format!("{} code", lang.name()), body: db::driver_code(lang, &t.sel.db, &t.sel.coll, &w), code: false }),
            Err(e) => self.err(e),
        }
    }

    /// Runs a pasted `db.<coll>.find(...)` or `db.<coll>.aggregate([...])`, switching collection if needed.
    fn run_shell(&mut self, text: &str) {
        let sh = match db::parse_shell(text) {
            Ok(s) => s,
            Err(e) => return self.err(e),
        };
        let Some(cur) = self.t().map(|t| t.sel.clone()) else { return self.err("Open a collection first.") };
        if sh.coll != cur.coll {
            self.open_collection(Sel { coll: sh.coll.clone(), ..cur }, true);
        }
        let t = self.tm().unwrap();
        t.selected = None;
        match sh.kind {
            ShellKind::Find(q) => {
                t.view = View::Documents;
                t.filter_txt = text.trim().to_string();
                t.sort_txt.clear();
                t.proj_txt.clear();
                t.query = q;
                t.page = 0;
                self.reload();
            }
            ShellKind::Aggregate(p) => {
                t.view = View::Aggregate;
                t.agg_txt = text.trim().to_string();
                self.run_pipeline(p);
            }
        }
    }

    /// Explains the Documents query or the Aggregate pipeline, whichever view is open.
    fn explain(&mut self) {
        let Some(t) = self.t() else { return };
        let what = match t.view {
            View::Documents if !db::is_shell(&t.filter_txt) => Ok(db::ExplainOf::Find(t.query.clone())),
            View::Aggregate if !db::is_shell(&t.agg_txt) => db::parse_pipeline(&t.agg_txt).and_then(|p| if db::has_write_stage(&p) { Err("Explain would run the write stage.".into()) } else { Ok(db::ExplainOf::Aggregate(p)) }),
            View::Documents | View::Aggregate => Err("Run the pasted query first, then explain it.".into()),
            _ => return,
        };
        match (what, self.target()) {
            _ if self.demo => self.ok("Offline demo: nothing to explain."),
            (Err(e), _) => self.err(e),
            (Ok(w), Some(target)) => {
                self.req += 1;
                self.explain_req = self.req;
                self.worker.explain(self.req, target, w);
            }
            _ => self.err("Not connected."),
        }
    }

    fn run_pipeline(&mut self, p: Vec<Document>) {
        self.tm().unwrap().agg_stage = None;
        if db::has_write_stage(&p) {
            if self.writable_target().is_some() {
                self.modal = Some(Modal::AggWrite(p));
            }
        } else if self.demo {
            self.ok("Offline demo: nothing to run.");
        } else {
            match self.target() {
                Some(target) => {
                    self.req += 1;
                    let req = self.req;
                    self.tm().unwrap().agg_req = req;
                    self.worker.aggregate(req, target, p);
                }
                None => self.err("Not connected."),
            }
        }
    }

    fn load_indexes(&mut self) {
        self.load_indexes_of(self.cur);
    }

    fn load_indexes_of(&mut self, i: usize) {
        if let Some(target) = self.tabs.get(i).and_then(|t| self.target_for(&t.sel)) {
            self.req += 1;
            let req = self.req;
            self.tabs[i].idx_req = req;
            // Only the Indexes view shows usage, and it costs a profiler query.
            if self.tabs[i].view == View::Indexes {
                self.worker.index_info(target.clone());
            }
            self.worker.indexes(req, target);
        }
    }

    /// Asks for the preview of every referenced id on the page, like the original's
    /// relatedDocuments (renderer line 107207).
    fn load_related(&mut self, req: u64) {
        let Some(t) = self.tabs.iter().find(|t| t.req == req) else { return };
        let Some(client) = self.conn(t.sel.conn).and_then(|c| c.client.clone()) else { return };
        let rels = self.prefs.relations.get(&format!("{}/{}", t.sel.conn, t.sel.db)).into_iter().flatten().filter(|r| r.coll == t.sel.coll && !r.preview.is_empty());
        for r in rels {
            let ids = db::values_at(&t.docs, &r.path);
            if !ids.is_empty() {
                self.worker.related(req, Target { client: client.clone(), db: t.sel.db.clone(), coll: r.target.clone(), timeout: self.timeout() }, ids, r.preview.clone());
            }
        }
    }

    /// Opens the referenced document in its collection's tab.
    fn go_ref(&mut self, conn: u64, db: String, coll: String, id: Bson) {
        self.open_collection(Sel { conn, db, coll }, false);
        let t = self.tm().unwrap();
        t.view = View::Documents;
        t.filter_txt = format!("{{ _id: {} }}", db::shell_text(&id));
        t.sort_txt.clear();
        t.proj_txt.clear();
        self.run_query();
    }

    fn analyze_relations(&mut self, conn: u64, client: Client, db: String, coll: String) {
        let all: Vec<String> = self.conn(conn).and_then(|c| c.dbs.iter().find(|d| d.0 == db)).and_then(|d| d.1.clone()).unwrap_or_default().into_iter().filter(|c| !c.starts_with("system.") && !self.views.contains(&(conn, db.clone(), c.clone()))).collect();
        let colls = if coll.is_empty() { all.clone() } else { vec![coll.clone()] };
        self.ok(format!("Analyzing data relations of {}…", if coll.is_empty() { db.clone() } else { format!("{db}.{coll}") }));
        self.worker.analyze_relations(conn, Target { client, db, coll, timeout: self.timeout() }, colls, all);
    }

    /// Something a restart would lose: unsaved edits, writes in flight, running scripts or tools.
    fn busy(&self) -> bool {
        !self.drafts.is_empty()
            || !self.saving.is_empty()
            || self.json_edit.is_some()
            || self.insert.is_some()
            || self.inline.is_some()
            || self.new_field.is_some()
            || self.modal.is_some()
            || self.tool.as_ref().is_some_and(|t| t.done.is_none())
            || self.tabs.iter().any(|t| t.shell.cancel.is_some() || t.compare.running || t.loading)
    }

    /// Checks GitHub 5 seconds after launch and then daily, like the original (main process
    /// setTimeout 5 s and setInterval 24 h, main.js line 33757); restarts into an update once idle.
    fn auto_update(&mut self, ctx: &egui::Context) {
        if self.demo || cfg!(debug_assertions) {
            return;
        }
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.last_input = std::time::Instant::now();
        }
        const FIRST_CHECK_S: u64 = 5;
        const CHECK_EVERY_S: u64 = 24 * 3600;
        let due = match self.update_checked {
            None => self.started.elapsed().as_secs() >= FIRST_CHECK_S,
            Some(t) => t.elapsed().as_secs() >= CHECK_EVERY_S,
        };
        if self.update_checked.is_none() && self.prefs.auto_update {
            ctx.request_repaint_after(std::time::Duration::from_secs(FIRST_CHECK_S));
        }
        if self.prefs.auto_update && self.updated.is_none() && due {
            self.check_update();
        }
        if self.updated.is_some() && self.prefs.auto_update {
            if self.last_input.elapsed().as_secs() >= 60 && !self.busy() {
                crate::update::restart();
            }
            // Wake up to test idleness even when nothing else repaints.
            ctx.request_repaint_after(std::time::Duration::from_secs(5));
        }
    }

    /// Menu items without a keyboard twin.
    #[cfg(target_os = "macos")]
    fn menu_act(&mut self, ctx: &egui::Context, a: crate::menu::Act) {
        use crate::menu::Act;
        match a {
            Act::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            // Same rule as Settings' "Check now": dev builds and the demo never install releases.
            Act::CheckUpdates => {
                if !self.demo && !cfg!(debug_assertions) && self.updated.is_none() {
                    self.check_update();
                }
                self.modal = Some(Modal::Settings);
            }
            Act::Export if !self.tabs.is_empty() => self.export(),
            Act::Insert if !self.tabs.is_empty() => self.insert = Some("{\n  \n}".into()),
            Act::Url(u) => ctx.open_url(egui::OpenUrl::new_tab(u)),
            _ => {}
        }
    }

    fn check_update(&mut self) {
        self.update_checked = Some(std::time::Instant::now());
        self.update_note = "Checking for updates…".into();
        self.worker.blocking(|| Msg::Update(crate::update::check_and_install()));
        // A running dump or restore keeps its program open; Windows cannot replace it then.
        if self.tool.as_ref().is_none_or(|t| t.done.is_some()) {
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::update_db_tools(&tools_dir())));
        }
    }

    fn reload(&mut self) {
        self.reload_of(self.cur);
    }

    fn reload_of(&mut self, i: usize) {
        if self.demo {
            return;
        }
        let Some(target) = self.tabs.get(i).and_then(|t| self.target_for(&t.sel)) else { return };
        // Each answer is matched to its tab by id; a slower older one never overwrites a newer one.
        self.req += 1;
        let req = self.req;
        let t = &mut self.tabs[i];
        t.req = req;
        t.loading = true;
        let (q, page, view, no_idx) = (t.query.clone(), t.page, t.view, t.indexes.is_empty());
        self.worker.find(req, target, q, page);
        // Indexes also feed the Sort history (as in the original), so load them once per tab.
        if view == View::Indexes || no_idx {
            self.load_indexes_of(i);
        }
    }

    fn push_undo(&mut self, a: UndoAction) {
        self.undo.insert(0, a);
        self.undo.retain(|a| a.at.elapsed().as_secs() <= db::UNDO_MAX_AGE_S);
        self.undo.truncate(db::UNDO_MAX);
    }

    fn apply_undo(&mut self, i: usize) {
        let a = &self.undo[i];
        let Some(c) = self.conn(a.conn) else { return };
        if !c.writable {
            self.err("Undo is a write: unlock writes on that connection first.");
            return;
        }
        let Some(client) = c.client.clone() else {
            self.err("That connection is closed.");
            return;
        };
        let a = self.undo.remove(i);
        let t = Target { client, db: a.db, coll: a.coll, timeout: self.timeout() };
        match a.kind {
            UndoKind::Insert { id } => self.worker.delete_by_id(t, id),
            UndoKind::InsertMany { ids } => self.worker.delete_ids(t, ids),
            UndoKind::Delete { old } => self.worker.insert(a.conn, t, old, None),
            UndoKind::DeleteMany { old } => self.worker.import(a.conn, t, old, None),
            UndoKind::Update { old, new } => self.worker.replace(a.conn, t, new, old, false),
        }
    }

    fn tab_by_req(&mut self, req: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.req == req)
    }

    fn selected_doc(&self) -> Option<&Document> {
        let t = self.t()?;
        match t.selected? {
            (false, i) => t.docs.get(i),
            // Shell and Import lend their rows to `agg_docs` only while their table draws, so
            // the side panel has to read the row from where it came from.
            (true, i) => match (t.view, &t.import.parsed) {
                (View::Shell, _) => t.shell.docs.get(t.shell.page * PAGE_SIZE as usize + i),
                (View::Import, Some(Ok(docs))) => docs.get(i),
                (View::Import, _) => None,
                _ => t.agg_docs.get(i),
            },
        }
    }

    fn draft_key(&self, d: &Document) -> String {
        let s = self.t().map(|t| format!("{}/{}/{}", t.sel.conn, t.sel.db, t.sel.coll)).unwrap_or_default();
        format!("{s}#{}", d.get("_id").map(db::compact).unwrap_or_default())
    }

    /// The document as it should be shown: with its unsaved edits, if any.
    fn shown_doc(&self, d: &Document) -> Option<Document> {
        if self.drafts.is_empty() {
            return None;
        }
        self.drafts.get(&self.draft_key(d)).map(|x| x.current.clone())
    }

    /// Keeps an edit as unsaved instead of writing it, so several edits go out in one save.
    fn stage(&mut self, d: &Document, new: db::Res<Document>) {
        let new = match new {
            Ok(n) => n,
            Err(e) => return self.err(e),
        };
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let original = self.drafts.get(&key).map(|x| x.original.clone()).unwrap_or_else(|| d.clone());
        self.inline = None;
        self.new_field = None;
        self.json_edit = None;
        if db::diff(&original, &new).is_empty() {
            self.drafts.remove(&key);
            return self.ok("No changes left on this document");
        }
        let n = db::diff(&original, &new).len();
        self.drafts.insert(key.clone(), Draft { conn: sel.conn, db: sel.db, coll: sel.coll, original, current: new });
        if self.prefs.auto_save && self.sel_writable() {
            self.write_draft(&key);
        } else {
            self.ok(format!("{n} unsaved change(s). Press Save on the document when ready."));
        }
    }

    fn save_draft(&mut self, key: &str) {
        let Some(dr) = self.drafts.get(key) else { return };
        if !self.conn(dr.conn).is_some_and(|c| c.writable) {
            let key = key.to_string();
            return self.modal = Some(Modal::Unlock(dr.conn, Some(Box::new(move |a: &mut App| a.save_draft(&key)))));
        }
        if self.prefs.confirm_save && !self.prefs.always_writable {
            let changes = db::diff(&dr.original, &dr.current);
            self.modal = Some(Modal::Save { tag: key.to_string(), changes });
        } else {
            self.write_draft(key);
        }
    }

    fn write_draft(&mut self, key: &str) {
        let Some(dr) = self.drafts.remove(key) else { return };
        let Some(target) = self.target_for(&Sel { conn: dr.conn, db: dr.db.clone(), coll: dr.coll.clone() }) else {
            self.drafts.insert(key.to_string(), dr);
            return self.err("Not connected.");
        };
        self.worker.save(key.to_string(), dr.conn, target, dr.original.clone(), dr.current.clone());
        self.saving.insert(key.to_string(), dr);
    }

    /// Unsaved documents of the open collection.
    fn tab_drafts(&self) -> Vec<String> {
        let Some(t) = self.t() else { return Vec::new() };
        let prefix = format!("{}/{}/{}#", t.sel.conn, t.sel.db, t.sel.coll);
        self.drafts.keys().filter(|k| k.starts_with(&prefix)).cloned().collect()
    }

}

impl eframe::App for App {
    /// Test switch for the demo picture: QUARRY_TEST_POINTER="x,y" hovers that point and
    /// "x,y,down" also holds the primary button there, to check hover and press states.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        #[cfg(not(target_os = "macos"))]
        let _ = ctx;
        #[cfg(target_os = "macos")]
        if let Some(m) = &mut self.menu {
            let rest = m.feed(ctx, raw);
            self.menu_acts.extend(rest);
        }
        if self.shot.is_none() {
            return;
        }
        let Ok(spec) = std::env::var("QUARRY_TEST_POINTER") else { return };
        let mut parts = spec.split(',');
        let (Some(Ok(x)), Some(Ok(y))) = (parts.next().map(str::parse::<f32>), parts.next().map(str::parse::<f32>)) else { return };
        let pos = egui::pos2(x, y);
        raw.events.push(egui::Event::PointerMoved(pos));
        if parts.next() == Some("down") {
            raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        LIGHT_MODE.store(ctx.theme() == egui::Theme::Light, std::sync::atomic::Ordering::Relaxed);
        // macOS leaves an app started from a terminal or Finder behind the frontmost one; the first
        // frames ask for focus once the window is visible (a no-op when it already has it).
        if ctx.cumulative_frame_nr() < 3 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        #[cfg(target_os = "macos")]
        for a in std::mem::take(&mut self.menu_acts) {
            self.menu_act(ctx, a);
        }
        // Screenshot mode must keep drawing even if the window is covered or hidden, or it never finishes.
        if self.shot.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint();
        }
        // Set every frame so connections added or reconnected later need no extra handling.
        if self.prefs.always_writable {
            self.conns.iter_mut().for_each(|c| c.writable = true);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frame_ms = frame.info().cpu_usage.unwrap_or(0.0) * 1000.0;
        // QUARRY_FRAME_LOG prints each frame's CPU time, for measuring with a fixture.
        if std::env::var("QUARRY_FRAME_LOG").is_ok() {
            eprintln!("frame_ms {:.3}", self.frame_ms);
        }
        self.poll();
        self.remember_tabs();
        let ctx = ui.ctx().clone();
        // AppKit resets the window buttons on every layout, so re-centre them in our header each frame.
        frame.set_traffic_lights_position(&ctx, egui::Rangef::new(0.0, HEADER_H), 16.0);
        self.top_bar(ui);
        self.status_bar(ui);
        self.query_log(ui);
        self.sidebar(ui);
        let view = self.t().map(|t| t.view);
        if !self.prefs.open_inline && self.selected_doc().is_some() && view != Some(View::Indexes) {
            self.inspector(ui);
        }
        egui::CentralPanel::default_margins().frame(egui::Frame::new().fill(BG()).inner_margin(egui::Margin { left: 12, right: 12, top: 8, bottom: 8 })).show(ui, |ui| {
            self.tab_strip(ui);
            if self.tabs.is_empty() {
                self.welcome(ui);
            } else {
                self.collection(ui);
            }
        });
        self.auto_update(&ctx);
        // Before the modals, so one opened by a shortcut draws this frame, and an Esc that closes a
        // modal is not also taken as a second Esc by `keys`.
        self.keys(&ctx);
        self.modals(&ctx);
        self.users_window(&ctx);
        self.tool_window(&ctx);
        self.toast(&ctx);
        self.screenshot(&ctx);
    }
}

impl App {

    fn danger(&mut self, action: Danger) {
        if let Danger::RemoveConnection(id) = action {
            // Demo ids overlap real ones, so removing a demo connection must not touch the Keychain.
            if !self.demo {
                if let Some(k) = keychain(id) {
                    let _ = k.delete_credential();
                }
                // ponytail: best effort, a failure only leaves an orphaned Keychain entry behind.
                for what in ["ssh-password", "ssh-passphrase", "proxy-password"] {
                    let _ = set_secret(id, what, "");
                }
            }
            self.conns.retain(|c| c.meta.id != id);
            self.tabs.retain(|t| t.sel.conn != id);
            self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
            if self.active == Some(id) {
                self.active = self.conns.first().map(|c| c.meta.id);
            }
            self.save_conns();
            return self.ok("Connection removed");
        }
        let conn_specific = match &action {
            Danger::DropDatabase { conn, .. } | Danger::DropCollAt { conn, .. } | Danger::DropUser { conn, .. } | Danger::Sync { conn } | Danger::Restore { conn, .. } => Some(*conn),
            _ => None,
        };
        if let Some(conn) = conn_specific {
            let Some(client) = self.conn(conn).filter(|c| c.writable).and_then(|c| c.client.clone()) else {
                return self.modal = Some(Modal::Unlock(conn, None));
            };
            match action {
                Danger::DropDatabase { db, .. } => {
                    self.tabs.retain(|t| !(t.sel.conn == conn && t.sel.db == db));
                    self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                    self.worker.db_change(conn, client, db.clone(), doc! { "dropDatabase": 1 }, format!("Dropped database {db}"));
                    if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                        c.dbs.retain(|d| d.0 != db);
                    }
                    self.dropping_db = Some((conn, db));
                }
                Danger::DropCollAt { db, coll, .. } => {
                    self.tabs.retain(|t| !(t.sel.conn == conn && t.sel.db == db && t.sel.coll == coll));
                    self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                    self.worker.db_change(conn, client, db, doc! { "drop": &coll }, format!("Dropped {coll}"));
                }
                Danger::DropUser { db, user, .. } => self.worker.db_change(conn, client, db, doc! { "dropUser": &user }, format!("Dropped user {user}")),
                Danger::Sync { .. } => {
                    let timeout = self.timeout();
                    let c = &mut self.tabs[self.cur].compare;
                    let Some(r) = c.result.take() else { return };
                    let insert = if c.insert { r.only_src } else { Vec::new() };
                    let replace = if c.replace { r.changed } else { Vec::new() };
                    let delete = if c.delete { r.only_dst.iter().filter_map(|d| d.get("_id").cloned()).collect() } else { Vec::new() };
                    self.worker.sync(Target { client, db: c.db.clone(), coll: c.coll.clone(), timeout }, insert, replace, delete);
                    self.ok("Syncing… Run Compare again afterwards to check.");
                }
                Danger::Restore { args, .. } => self.start_tool(true, "Restore".into(), args),
                _ => unreachable!(),
            }
            return;
        }
        let Some((conn, target)) = self.writable_target() else { return };
        match action {
            Danger::DeleteMatching(f) => self.worker.delete_many(target, f),
            Danger::DropIndex(name) => self.worker.drop_index(target, name),
            Danger::Validator(cmd) => self.worker.db_change(conn, target.client, target.db, cmd, "Validator updated".into()),
            Danger::DropCollection => {
                // The sidebar's drop also reloads the collection list, which keeps a refused drop visible.
                let Sel { conn, db, coll } = self.t().unwrap().sel.clone();
                self.danger(Danger::DropCollAt { conn, db, coll });
            }
            Danger::RenameMany { filter, from, to } => self.worker.rename_many(target, filter, from, to),
            Danger::UpdateMany { filter, stages } => self.worker.update_many(target, filter, stages),
            Danger::Upsert { key } => {
                let conn = self.t().unwrap().sel.conn;
                if let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                    self.worker.import(conn, target, docs, Some(key));
                }
            }
            Danger::RemoveConnection(_) | Danger::DropDatabase { .. } | Danger::DropCollAt { .. } | Danger::DropUser { .. } | Danger::Sync { .. } | Danger::Restore { .. } => unreachable!(),
        }
    }
}

fn human_bytes(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

fn group(n: &str) -> String {
    let mut out = String::new();
    for (i, ch) in n.chars().enumerate() {
        if i > 0 && (n.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_refs() {
        assert_eq!(collection_ref("users"), "users");
        assert_eq!(collection_ref("my-coll"), "db.collection(\"my-coll\")");
        assert_eq!(collection_ref("2024"), "db.collection(\"2024\")");
        for word in ["default", "class", "new", "delete", "import", "await"] {
            assert_eq!(collection_ref(word), format!("db.collection(\"{word}\")"));
        }
        assert_eq!(query_script("default", ""), "async function run() {\n\treturn await db.collection(\"default\").find({})\n}");
    }

    #[test]
    fn relation_files() {
        let list = vec![Relation { coll: "orders".into(), path: "items.productId".into(), target: "products".into(), preview: "title".into() }];
        assert!(relations_import(&relations_export("shop", &list)).unwrap() == list);
        let mingo = r#"{"version":"1.0","database":"shop","relations":[{"collection":"orders","path":"items[].productId","referencedCollection":"products","previewField":"title"}]}"#;
        assert!(relations_import(mingo).unwrap() == list);
        assert!(relations_import(r#"{"relations":[]}"#).is_err());
    }

}
