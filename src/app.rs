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
mod auto_update;
mod chrome;
mod column_menu;
mod compare;
mod config;
mod conn_form;
mod danger;
mod demo;
mod documents;
mod drafts;
mod explain;
mod export;
mod import;
mod rail;
mod indexes;
mod log;
mod modals;
mod palette;
mod poll;
mod query;
mod relations;
mod run;
mod schema;
mod server;
mod sql_server;
mod settings;
mod shell;
mod shell_attach;
mod shell_ls;
mod sidebar;
mod sql_ac;
mod sql_create;
mod sql_structure;
mod sql_alter;
mod sql_rel;
mod sql_trig;
mod sql_edit;
mod sql_bar;
mod sql_views;
mod sql_query;
mod sql_import;
mod sql_dump;
mod sql_users;
mod state;
mod table;
mod tabs;
mod tools;
mod tree;
mod undo;
mod users;
mod watch;
use column_menu::*;
use config::*;
use documents::*;
use shell::*;
use shell_attach::*;
use palette::*;
use sidebar::*;
use state::*;
use query::*;
use tools::*;
use tree::*;
use users::*;
use indexes::*;
use server::*;
use sql_server::*;
use watch::*;
use drafts::*;
use sql_edit::*;
use sql_alter::*;
use sql_create::*;
use sql_rel::*;
use table::Rows;

const SYSTEM_DBS: [&str; 3] = ["admin", "config", "local"];
/// Columns shown before the user picks their own; drawing every field of a wide collection is slow.
const DEFAULT_COLS: usize = 8;

/// Shortcuts listed by Cmd+/. Cmd is Ctrl on Linux and Windows.
const SHORTCUTS: [(&str, &str); 24] = [
    ("Cmd+K, Cmd+T", "Jump to a connection, database or collection"),
    ("Cmd+R", "Reload the current tab"),
    ("Cmd+Shift+R", "Refresh the connection's databases"),
    ("Cmd+W", "Close the tab"),
    ("Cmd+Shift+T", "Reopen the last closed tab"),
    ("Cmd+1 to Cmd+8", "Go to that tab"),
    ("Cmd+9", "Go to the last tab"),
    ("Ctrl+Tab, Cmd+Shift+]", "Next tab"),
    ("Ctrl+Shift+Tab, Cmd+Shift+[", "Previous tab"),
    ("Cmd+[, Cmd+]", "Back and forward through the tabs you visited"),
    ("Cmd+Alt+1 to Cmd+Alt+8", "Documents, Aggregate, Indexes, Schema, Import, Compare, Shell, Watch"),
    ("Cmd+F", "Focus the query"),
    ("Cmd+Shift+F", "Find a collection in the sidebar"),
    ("Up, Down", "Select the previous or next document"),
    ("PageUp, PageDown", "Previous or next page"),
    ("Enter", "Run the query"),
    ("Cmd+E", "Explain the query, pipeline or SQL statement"),
    ("Cmd+Enter", "Run the pipeline, script or SQL statement under the caret"),
    ("Cmd+Shift+Enter", "Run every statement of a SQL Query view"),
    ("Esc", "Close or deselect"),
    ("Cmd+,", "Settings"),
    ("Cmd+N", "New connection"),
    ("Cmd+L", "Show or hide the query log"),
    ("Cmd+/", "This list"),
];

pub struct App {
    worker: Worker,
    rx: Receiver<Msg>,
    conns: Vec<Conn>,
    active: Option<u64>,
    open_dbs: HashSet<(u64, String)>,
    /// Sidebar TABLES / VIEWS groups folded shut, by connection, database and whether it is the views group.
    side_closed: HashSet<(u64, String, bool)>,
    /// SQL databases whose table list was asked for; lists load when a database opens, not all at connect.
    sql_asked: HashSet<(u64, String)>,
    tabs: Vec<Tab>,
    cur: usize,
    req: u64,
    json_edit: Option<JsonEdit>,
    drafts: HashMap<String, Draft>,
    saving: HashMap<String, Draft>,
    /// Paths changed in the document being drawn, for tinting.
    changed: HashSet<String>,
    inline: Option<Inline>,
    /// The SQL grid cell being edited in place.
    sql_cell: Option<CellEdit>,
    new_field: Option<NewField>,
    /// Pending expand/collapse from the tree menu: document, path prefix, open.
    fold: Option<(egui::Id, Vec<String>, bool)>,
    fold_busy: bool,
    /// Set while drawing a document: whether any object or array section is closed, None when it
    /// has none. The document's Expand all button reads last frame's answer to pick its label.
    fold_seen: Option<bool>,
    /// Key column width and full row span of the tree being drawn, so values line up and hover bands cover the whole card.
    key_w: f32,
    char_w: f32,
    tree_x: egui::Rangef,
    insert: Option<String>,
    /// Database hidden from the sidebar while its drop runs, to show again if the drop fails.
    dropping_db: Option<(u64, String)>,
    /// Collections being written by a duplicate, as (connection, database, new name), shown with a spinner.
    copying: std::collections::HashSet<(u64, String, String)>,
    /// The Query result being streamed to a file or the clipboard, shown in the status bar with a Cancel.
    export_job: Option<export::ExportJob>,
    /// A CSV import or a script file running, shown in the status bar with a Cancel.
    sql_job: Option<sql_dump::SqlJob>,
    prefs: UiPrefs,
    new_col: String,
    col_filter: String,
    /// Column data actions cover the whole query instead of the loaded page.
    col_scope_query: bool,
    #[cfg(target_os = "macos")]
    menu: Option<crate::menu::MenuBar>,
    /// Menu picks that are not key presses, handled at the start of the next frame.
    #[cfg(target_os = "macos")]
    menu_acts: Vec<crate::menu::Act>,
    new_pkg: String,
    new_lib: String,
    /// Views per (connection, database), shown differently in the sidebar.
    views: HashSet<(u64, String, String)>,
    /// Tables and columns of SQL databases, per (connection, database), for autocomplete. Loaded when a
    /// SQL input there first has focus, and again on the sidebar's Refresh.
    sql_schema: HashMap<(u64, String), Arc<db::sql::schema::Schema>>,
    sql_schema_asked: HashSet<(u64, String)>,
    /// Looked up column values per (connection, database), keyed `table.column`, and the lookups already sent.
    sql_values: HashMap<(u64, String), Fields>,
    sql_values_asked: HashSet<(u64, String, String, String, Option<String>)>,
    users: Option<UsersWin>,
    sql_users: Option<sql_users::SqlUsersWin>,
    server: Option<ServerWin>,
    sql_server: Option<SqlServerWin>,
    /// Kill requests sent from the Server window and not answered yet. Kept here so an answer that arrives after
    /// the window was closed still reaches the person.
    sql_kills: HashSet<u64>,
    /// The answer to a connection form Test, with the request id it answers.
    conn_test: Option<(u64, db::Res<String>)>,
    /// What the last Test learned from a SQL server, for the Advanced tab to offer.
    test_info: Option<(u64, db::sql::TestInfo)>,
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
    /// Field values fetched from the server for suggestions, per collection key then path. Kept on
    /// disk, so a new session suggests them before its own fetch comes back.
    values: HashMap<String, HashMap<String, Vec<Bson>>>,
    /// (collection key, path, typed text) already fetched this session; no text for the sample.
    values_asked: HashSet<(String, String, Option<String>)>,
    /// (collection key, path) asked and not answered yet.
    values_pending: HashSet<(String, String)>,
    /// (collection key, path) whose typed text is not looked up, and why.
    values_note: HashMap<(String, String), &'static str>,
    /// A string value being typed: when to look up values starting with it, collection key, path, text.
    values_typed: Option<(std::time::Instant, String, String, String)>,
    /// Shell editor language server, started on the first edit of a script; it quits when idle,
    /// so it only runs while someone writes JavaScript.
    js: shell_ls::JsState,
    tree_filter: String,
    tools_installing: bool,
    /// Tree row the palette jumped to, as (connection, database, collection), scrolled into view once drawn.
    reveal: Option<(u64, String, Option<String>)>,
    /// Ids of the open connections in the order they were first seen open, for the rail shortcuts.
    rail_order: Vec<u64>,
    /// Scroll the selected document row into view on its next draw, after keyboard selection.
    scroll_sel: bool,
    /// The active tab the tab strip last scrolled to; it scrolls again when this differs.
    tab_seen: Option<usize>,
    demo: bool,
    tool: Option<ToolRun>,
    /// CPU time of the last frame in milliseconds, for the frame rate readout.
    frame_ms: f32,
    /// Last memory and CPU sample for the status bar: when, total CPU time then, CPU percent, bytes.
    usage: (std::time::Instant, Duration, f32, Option<u64>),
    /// Frame rate readout: seconds and frames counted so far, and the rate last shown.
    fps: (f32, u32, f32),
    /// Measured column widths per result set and column.
    width_cache: HashMap<(u64, usize, usize, String, Option<String>, bool), f32>,
    /// Recently closed tabs, newest first.
    closed: Vec<Tab>,
    /// Uids of tabs left and tabs gone back from, for Cmd+[ and Cmd+], newest last.
    nav_back: Vec<u64>,
    nav_fwd: Vec<u64>,
    /// Version installed by the updater, waiting for a restart.
    updated: Option<String>,
    update_note: String,
    update_checked: Option<std::time::Instant>,
    last_input: std::time::Instant,
    started: std::time::Instant,
    shot: Option<(PathBuf, u32, Arc<Mutex<Option<egui::ColorImage>>>)>,
}

/// Top-level fields of the shown documents, `_id` first, in the order they appear.
fn auto_fields<'a>(docs: impl IntoIterator<Item = impl Into<DocRef<'a>>>) -> Vec<String> {
    let mut cols = vec!["_id".to_string()];
    let mut seen: HashSet<&str> = HashSet::from(["_id"]);
    for d in docs {
        for (k, _) in d.into().fields() {
            if seen.insert(k) {
                cols.push(k.to_string());
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
            side_closed: HashSet::new(),
            sql_asked: HashSet::new(),
            tabs: Vec::new(),
            cur: 0,
            req: 0,
            json_edit: None,
            drafts: HashMap::new(),
            saving: HashMap::new(),
            changed: HashSet::new(),
            inline: None,
            sql_cell: None,
            new_field: None,
            fold: None,
            fold_busy: false,
            fold_seen: None,
            key_w: 0.0,
            char_w: 0.0,
            tree_x: egui::Rangef::NOTHING,
            insert: None,
            dropping_db: None,
            copying: Default::default(),
            export_job: None,
            sql_job: None,
            prefs: UiPrefs::default(),
            new_col: String::new(),
            col_filter: String::new(),
            col_scope_query: false,
            // Screenshot runs stay off the real menu bar.
            #[cfg(target_os = "macos")]
            menu: (!demo || std::env::var("QUARRY_DEMO").is_ok()).then(|| crate::menu::MenuBar::new(&cc.egui_ctx)),
            #[cfg(target_os = "macos")]
            menu_acts: Vec::new(),
            new_pkg: String::new(),
            new_lib: String::new(),
            views: HashSet::new(),
            sql_schema: HashMap::new(),
            sql_schema_asked: HashSet::new(),
            sql_values: HashMap::new(),
            sql_values_asked: HashSet::new(),
            users: None,
            sql_users: None,
            server: None,
            sql_server: None,
            sql_kills: HashSet::new(),
            conn_test: None,
            test_info: None,
            idx_info: None,
            undo: Vec::new(),
            explain_req: 0,
            modal: None,
            status: ("Ready. All connections start read-only.".into(), false),
            toast: None,
            ac: Ac::default(),
            values: HashMap::new(),
            values_asked: HashSet::new(),
            values_pending: HashSet::new(),
            values_note: HashMap::new(),
            values_typed: None,
            js: Default::default(),
            status_at: std::time::Instant::now(),
            log: LogUi::default(),
            tree_filter: String::new(),
            tools_installing: false,
            reveal: None,
            rail_order: Vec::new(),
            scroll_sel: false,
            tab_seen: None,
            demo,
            tool: None,
            width_cache: HashMap::new(),
            frame_ms: 0.0,
            usage: (std::time::Instant::now(), Duration::ZERO, 0.0, None),
            fps: (0.0, 0, 0.0),
            closed: Vec::new(),
            nav_back: Vec::new(),
            nav_fwd: Vec::new(),
            updated: None,
            update_note: String::new(),
            update_checked: None,
            last_input: std::time::Instant::now(),
            started: std::time::Instant::now(),
            shot: shot.map(|p| (p, 0, Arc::new(Mutex::new(None)))),
        };
        // Test switch: the frame rate, memory and CPU readouts, which demo mode leaves off.
        if demo && std::env::var("QUARRY_TEST_STATUS").is_ok() {
            (app.prefs.show_fps, app.prefs.show_mem, app.prefs.show_cpu) = (true, true, true);
        }
        // Test fixture: QUARRY_TEST_JSON=<file of Extended JSON documents> makes screenshot mode
        // show those documents through the same message path real query results take. A .jsonl
        // file (one document per line) is read one document at a time like a cursor, so memory
        // benchmarks measure what a query leaves behind rather than the parse of one big array.
        let fixture = std::env::var("QUARRY_TEST_JSON").ok().filter(|_| demo);
        if let Some(path) = fixture {
            app.demo = false;
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<db::Row> = if path.ends_with(".jsonl") {
                text.lines().filter_map(|l| db::parse_doc(l).ok()).filter_map(|d| db::Row::from_doc(&d).ok()).collect()
            } else {
                match db::parse_value(&text) {
                    Ok(Bson::Array(a)) => a.iter().filter_map(Bson::as_document).filter_map(|d| db::Row::from_doc(d).ok()).collect(),
                    _ => Vec::new(),
                }
            };
            drop(text);
            let sel = Sel { conn: 1, db: "fixture".into(), coll: "documents".into() };
            app.conns.push(Conn { meta: ConnMeta { id: 1, name: "Fixture".into(), ..Default::default() }, client: None, sql: None, status: Status::Ready, writable: false, dbs: vec![("fixture".into(), Some(vec!["documents".into()]))], masked_uri: String::new(), tunnel: None, server: Default::default() });
            app.active = Some(1);
            app.open_dbs.insert((1, "fixture".into()));
            app.open_collection(sel, false);
            app.req += 1;
            app.tabs[0].req = app.req;
            let n = rows.len() as u64;
            app.worker.send(Msg::Docs { req: app.req, res: Ok((rows, n, 3)) });
        } else if demo {
            app.load_demo();
            // Test switches: the demo picture in either theme regardless of the system, and with
            // the find bar open.
            if std::env::var("QUARRY_TEST_LIGHT").is_ok() {
                Appearance::Light.apply(&cc.egui_ctx);
            } else if std::env::var("QUARRY_TEST_DARK").is_ok() {
                Appearance::Dark.apply(&cc.egui_ctx);
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
            app.values = load_values();
            app.prefs.appearance.apply(&cc.egui_ctx);
            for meta in metas {
                let masked = keychain(meta.id).and_then(|k| k.get_password().ok()).map(|u| db::mask_uri(&u)).unwrap_or_default();
                app.conns.push(Conn { meta, client: None, sql: None, status: Status::Idle, writable: false, dbs: Vec::new(), masked_uri: masked, tunnel: None, server: Default::default() });
            }
            app.active = app.conns.first().map(|c| c.meta.id);
            // Values of deleted connections go with them.
            app.values.retain(|k, _| app.conns.iter().any(|c| k.starts_with(&format!("{}/", c.meta.id))));
            // Reopen the tabs of the last session; they load once their connection is connected.
            for st in app.prefs.open_tabs.clone() {
                if app.conn(st.conn).is_none() {
                    continue;
                }
                let engine = app.conn(st.conn).map(|c| c.meta.engine).unwrap_or_default();
                let mut t = Tab::new(Sel { conn: st.conn, db: st.db, coll: st.coll }).for_engine(engine);
                t.view = match st.view.as_str() {
                    "Aggregate" => View::Aggregate,
                    "Indexes" => View::Indexes,
                    "Shell" => View::Shell,
                    "Schema" => View::Schema,
                    "Import" => View::Import,
                    "Compare" => View::Compare,
                    "Watch" => View::Watch,
                    "Structure" => View::Structure,
                    "Relations" => View::Relations,
                    "Triggers" => View::Triggers,
                    "Info" => View::Info,
                    "Sql" => View::Sql,
                    _ => View::Documents,
                };
                (t.filter_txt, t.sort_txt, t.proj_txt, t.shell.code) = (st.filter, st.sort, st.project, st.shell);
                // A connection deleted since the last session cannot be attached any more.
                t.shell.attached = st.attached.into_iter().filter(|a| app.conn(a.conn).is_some()).collect();
                if let (Some(s), Ok(c)) = (t.sql.as_mut(), serde_json::from_str(&st.clauses)) {
                    s.bar.c = c;
                }
                if let Some(s) = t.sql.as_mut() {
                    s.query = st.query;
                }
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
            let current = env!("CARGO_PKG_VERSION");
            if let Some(id) = app.prefs.last_conn.filter(|&id| app.conn(id).is_some()) {
                app.active = Some(id);
            }
            // The active tab wins over the saved connection, so the sidebar starts on the tab's connection like after a tab switch.
            if let Some(t) = app.tabs.get(app.cur) {
                app.active = Some(t.sel.conn);
            }
            // Every connection open last time comes back open, in its rail order. The connections of reopened tabs
            // count too, for settings saved before open_conns existed.
            let mut seen = std::collections::HashSet::new();
            let open: Vec<u64> = app.prefs.open_conns.iter().copied().chain(app.active).chain(app.tabs.iter().map(|t| t.sel.conn)).filter(|id| app.conn(*id).is_some() && seen.insert(*id)).collect();
            app.rail_order = open.clone();
            for id in open {
                app.connect(id);
            }
            if app.prefs.last_version != current {
                // A fresh install has no ui.json, so it skips the notes. Settings from 0.2.0 and
                // earlier lack last_version, so updating from those shows them.
                if ui_json.is_some()
                    && let Some(body) = crate::update::changelog(&app.prefs.last_version, current)
                {
                    // Several releases come each under a "Quarry x.y.z" heading.
                    let title = if body.starts_with("Quarry ") { format!("What's new since Quarry {}", app.prefs.last_version) } else { format!("What's new in Quarry {current}") };
                    app.modal = Some(Modal::Text { title, body, code: false });
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

}

impl eframe::App for App {
    /// Test switch for the demo picture: QUARRY_TEST_POINTER="x,y" hovers that point,
    /// "x,y,down" also holds the primary button there, to check hover and press states, and
    /// "x,y,right" right-clicks there once on frame 60, so the capture shows the context menu;
    /// "x,y,right,x2,y2" then moves the pointer to x2,y2 to open a submenu.
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
        let mut pos = egui::pos2(x, y);
        let mode = parts.next();
        // "x,y,right,x2,y2": after the menu opened, the pointer moves on to x2,y2, to open a submenu.
        if let (Some("right"), Some(Ok(x2)), Some(Ok(y2))) = (mode, parts.next().map(str::parse::<f32>), parts.next().map(str::parse::<f32>)) {
            if self.shot.as_ref().is_some_and(|s| s.1 > 62) {
                pos = egui::pos2(x2, y2);
            }
        }
        raw.events.push(egui::Event::PointerMoved(pos));
        match mode {
            Some("down") => raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }),
            // One right-click partway through, so the capture shows the context menu settled.
            Some("right") if self.shot.as_ref().is_some_and(|s| s.1 == 60) => {
                for pressed in [true, false] {
                    raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() });
                }
            }
            _ => {}
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        LIGHT_MODE.store(ctx.theme() == egui::Theme::Light, std::sync::atomic::Ordering::Relaxed);
        // macOS leaves an app started from a terminal or Finder behind the frontmost one; the first
        // frames ask for focus once the window is visible (a no-op when it already has it).
        if ctx.cumulative_frame_nr() < 3 && crate::takes_focus() {
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
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.stop_hidden_watches();
        self.remember_tabs();
        // AppKit resets the window buttons on every layout, so re-centre them in our header each frame.
        frame.set_traffic_lights_position(&ctx, egui::Rangef::new(0.0, HEADER_H), 16.0);
        self.top_bar(ui);
        self.status_bar(ui);
        self.rail(ui);
        if !self.prefs.sidebar_hidden {
            self.sidebar(ui);
        }
        self.query_log(ui);
        if !self.prefs.open_inline && self.selected_doc().is_some() {
            self.inspector(ui);
        }
        // After the side panels, so the strip spans only the workspace.
        if !self.tabs.is_empty() {
            self.tab_strip(ui);
        }
        egui::CentralPanel::default_margins().frame(egui::Frame::new().fill(BG()).inner_margin(egui::Margin { left: 20, right: 20, top: 10, bottom: 12 })).show(ui, |ui| {
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
        self.sql_users_window(&ctx);
        self.server_window(&ctx);
        self.sql_server_window(&ctx);
        self.tool_window(&ctx);
        self.toast(&ctx);
        self.screenshot(&ctx);
    }
}
fn human_bytes(n: usize) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

pub(crate) fn group(n: &str) -> String {
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

}
