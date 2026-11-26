use crate::ui::{cells::*, complete::*, dialog::*, editor::*, glyphs, pickers, theme::*, widgets::*};
use crate::db::{self, Msg, PAGE_SIZE, Target, UndoAction, UndoKind, Worker};
use quarry_core::core::{Core, Front};
use quarry_core::action::{Action, ConnA, GroupA, Secret, SideA, SqlA, SqlAskA, TabA};
use quarry_core::cmd::{Chord, Cmd};
use eframe::egui::{self, Color32, FontId, Key, RichText, Sense, Stroke, text::LayoutJob};
use mongodb::Client;
use mongodb::bson::{Bson, Document, doc, oid::ObjectId};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod agents;
mod aggregate;
mod auto_update;
mod chrome;
mod cloud_sql;
mod column_menu;
mod compare;
pub(crate) mod config;
mod conn_form;
mod conn_icon;
mod conns_panel;
mod crumbs;
mod conn_pg_advanced;
mod danger;
mod demo;
mod demo_pg;
mod documents;
mod drafts;
mod errors;
mod exec;
mod explain;
mod export;
mod group_dialog;
mod groups;
mod import;
mod rail;
mod indexes;
mod library;
mod log;
mod mem;
mod window_buttons;
mod modals;
mod mongo_create;
mod palette;
mod poll;
mod query;
mod relations;
mod results_bar;
mod restore;
mod run;
mod schema;
mod server;
mod settings;
mod sql;
mod shell;
mod shell_attach;
mod shell_ls;
mod sidebar;
mod state;
mod table;
mod tabs;
mod tools;
mod tree;
mod undo;
mod vim_nav;
mod users;
mod value_edit;
#[cfg(test)]
mod ui_tests;
mod update_pill;
mod watch;
mod whats_new;
use column_menu::*;
use auto_update::{HELD, UpdBtn, UpdBtnUi};
use whats_new::WhatsNew;
use errors::*;
use config::*;
use groups::{Group, Groups, Place};
use library::picker::{PickKind, picker_button};
use documents::*;
use shell::*;
use shell_attach::*;
use palette::*;
use sidebar::*;
use state::*;
use query::*;
use tools::*;
use tree::*;
use indexes::*;
use server::*;
use sql::server::*;
use sql::pg_server::*;
use sql::routines::RAct;
use watch::*;
use tabs::new_tab_click;
use drafts::*;
use sql::edit::*;
use sql::create::*;
use table::Rows;

const SYSTEM_DBS: [&str; 3] = ["admin", "config", "local"];

/// egui's shortcut for a chord of the registry. `Cmd` is Command on macOS and Ctrl elsewhere, as egui's COMMAND is.
pub(crate) fn shortcut(c: &Chord) -> egui::KeyboardShortcut {
    let mut m = egui::Modifiers::NONE;
    for (on, k) in [(c.cmd, egui::Modifiers::COMMAND), (c.ctrl, egui::Modifiers::CTRL), (c.shift, egui::Modifiers::SHIFT), (c.alt, egui::Modifiers::ALT)] {
        if on {
            m = m | k;
        }
    }
    egui::KeyboardShortcut::new(m, Key::from_name(c.key).expect("the registry spells keys as egui does"))
}
pub struct App {
    /// Everything that is not drawing: connections, tabs, windows, preferences and the worker.
    core: Front,
    /// Drawing-only state that Core has no use for.
    ui: UiState,
    /// Pending expand/collapse from the tree menu: document, path prefix, open.
    fold: Option<(egui::Id, Vec<String>, bool)>,
    tree_x: egui::Rangef,
    #[cfg(target_os = "macos")]
    menu: Option<crate::menu::MenuBar>,
    /// Menu picks that call a command, handled at the start of the next frame.
    #[cfg(target_os = "macos")]
    menu_acts: Vec<Cmd>,
    modal: Option<Modal>,
    /// An answer that arrived while a dialog was open, shown once that dialog closes.
    late_modals: Vec<Modal>,
    /// Shell editor language server, started on the first edit of a script; it quits when idle,
    /// so it only runs while someone writes JavaScript.
    js: shell_ls::JsState,
    /// Vim navigation outside the editors: pending keys, the grid action they asked for and what the grid drew.
    vnav: vim_nav::VimNav,
    /// The current scroll gesture for `prefs.lock_scroll_axis`: distance so far, locked axis (true is vertical) and last scroll time.
    scroll_gesture: (egui::Vec2, Option<Option<bool>>, f64, egui::Vec2),
    shot: Option<(PathBuf, u32, Arc<Mutex<Option<egui::ColorImage>>>)>,
    /// Text an action put on the clipboard, copied by the next `logic()`.
    clip: Option<String>,
    /// The shortest wait an action asked to be drawn again after, read back by `take_repaint`.
    repaint: Option<f64>,
    /// `run_fx` is carrying out effects, so an action one of them runs leaves its own effects to that same drain.
    in_fx: bool,
    /// An approval waits while the window has no focus: the next `logic()` asks for the user's attention.
    attention: bool,
}

pub(crate) use crate::db::collection_ref;

impl App {
    /// The empty state both `new` and the test constructor start from, so a new field is added in one place.
    fn blank(cc: &eframe::CreationContext<'_>, demo: bool, menu: bool, shot: Option<PathBuf>) -> Self {
        // The demo and fixture modes start here; tests keep their fake vault, which one test going offline would empty for all.
        if demo && !cfg!(test) {
            crate::vault::go_offline();
        }
        // Only macOS has a native menu bar.
        #[cfg(not(target_os = "macos"))]
        let _ = menu;
        let (tx, rx) = channel();
        let worker = Worker::new(tx, { let ctx = cc.egui_ctx.clone(); std::sync::Arc::new(move || ctx.request_repaint()) });
        Self {
            core: Front::new(Core::new(worker, rx, demo)),
            ui: Default::default(),
            fold: None,
            tree_x: egui::Rangef::NOTHING,
            #[cfg(target_os = "macos")]
            menu: menu.then(|| crate::menu::MenuBar::new(&cc.egui_ctx)),
            #[cfg(target_os = "macos")]
            menu_acts: Vec::new(),
            modal: None,
            late_modals: Vec::new(),
            js: Default::default(),
            vnav: Default::default(),
            scroll_gesture: Default::default(),
            shot: shot.map(|p| (p, 0, Arc::new(Mutex::new(None)))),
            clip: None,
            repaint: None,
            in_fx: false,
            attention: false,
        }
    }

    /// An app with no disk, keychain, menu bar or demo data, for the headless UI tests. Connections are added by the test.
    #[cfg(test)]
    pub fn for_test(cc: &eframe::CreationContext<'_>) -> Self {
        style(&cc.egui_ctx, Look::default());
        let mut app = Self::blank(cc, false, false, None);
        app.core.setup().ephemeral = true;
        // A save runs on the click alone, and no timer-driven save can change the draft under a test.
        (app.core.setup().prefs.confirm_save, app.core.setup().prefs.auto_save) = (false, false);
        app
    }

    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>) -> Self {
        style(&cc.egui_ctx, Look::default());
        // QUARRY_DEMO=1 opens the offline demo to click around in, without screenshot mode closing it.
        let demo = shot.is_some() || std::env::var("QUARRY_DEMO").is_ok();
        // Screenshot runs stay off the real menu bar.
        let mut app = Self::blank(cc, demo, !demo || std::env::var("QUARRY_DEMO").is_ok(), shot);
        app.ui.win_buttons = window_buttons::install(cc);
        // shortcut: the window opens decorated and loses them on the first frame when opted in, so it may flash; build the viewport from ui.json to avoid that.
        #[cfg(target_os = "linux")]
        app.set_titlebar(&cc.egui_ctx);
        // Test switch: the frame rate, memory and CPU readouts, which demo mode leaves off.
        if demo && std::env::var("QUARRY_TEST_STATUS").is_ok() {
            (app.core.setup().prefs.show_fps, app.core.setup().prefs.show_mem, app.core.setup().prefs.show_cpu) = (true, true, true);
        }
        // Test fixture: QUARRY_TEST_JSON=<file of Extended JSON documents> makes screenshot mode
        // show those documents through the same message path real query results take. A .jsonl
        // file (one document per line) is read one document at a time like a cursor, so memory
        // benchmarks measure what a query leaves behind rather than the parse of one big array.
        let fixture = std::env::var("QUARRY_TEST_JSON").ok().filter(|_| demo);
        if let Some(path) = fixture {
            app.core.setup().demo = false;
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
            let sel = Sel { conn: 1, db: "fixture".into(), coll: "documents".into(), ..Default::default() };
            app.core.setup().conns.push(Conn { dbs: vec![("fixture".into(), Some(vec!["documents".into()]))], ..Conn::new(ConnMeta { id: 1, name: "Fixture".into(), ..Default::default() }, Status::Ready) });
            app.core.setup().active = Some(1);
            app.core.setup().open_dbs.insert((1, "fixture".into()));
            app.open_collection(sel, Open::Here);
            app.core.setup().req += 1;
            app.core.setup().tabs[0].req = app.core.setup().req;
            let n = rows.len() as u64;
            let req = app.core.req;
            app.core.setup().loopback(Msg::Docs { req, res: Ok((rows, n, 3)), approx: false });
        } else if demo {
            app.load_demo();
            // Test switches: the demo picture in either theme regardless of the system, and with
            // the find bar open.
            if std::env::var("QUARRY_TEST_LIGHT").is_ok() {
                Appearance::Light.apply(&cc.egui_ctx);
            } else if std::env::var("QUARRY_TEST_DARK").is_ok() {
                Appearance::Dark.apply(&cc.egui_ctx);
            }
            if std::env::var("QUARRY_TEST_READING").is_ok() {
                let p = &mut app.core.setup().prefs;
                (p.ui_scale, p.soft, p.reading_line, p.group_digits, p.keep_toasts) = (1.25, true, true, true, true);
                p.ui_font = "OpenDyslexic".into();
            }
            // QUARRY_TEST_THEME=<name> shows that theme in its own mode; the font switches take a name from the Settings lists.
            if let Ok(name) = std::env::var("QUARRY_TEST_THEME") {
                let light = LIGHT_THEMES.iter().any(|t| t.name == name);
                *(if light { &mut app.core.setup().prefs.light_theme } else { &mut app.core.setup().prefs.dark_theme }) = name;
                app.core.setup().prefs.appearance = if light { Appearance::Light } else { Appearance::Dark };
                app.core.setup().prefs.appearance.apply(&cc.egui_ctx);
            }
            if let Ok(a) = std::env::var("QUARRY_TEST_ACCENT") {
                app.core.setup().prefs.accent = a;
            }
            if let Ok(f) = std::env::var("QUARRY_TEST_UI_FONT") {
                app.core.setup().prefs.ui_font = f;
            }
            if let Ok(f) = std::env::var("QUARRY_TEST_DATA_FONT") {
                app.core.setup().prefs.data_font = f;
            }
            style(&cc.egui_ctx, app.core.setup().prefs.look());
            if let Ok(q) = std::env::var("QUARRY_TEST_FIND") {
                let f = Find { open: true, query: q, ..Default::default() };
                cc.egui_ctx.data_mut(|d| d.insert_temp(egui::Id::unique("json-editor-text").with("find"), f));
            }
        } else {
            crate::update::cleanup();
            let (metas, broken_conns) = parse_config_list::<ConnMeta>(&config_dir().join("connections.json"));
            let ui_json = config_dir().join("ui.json").exists().then_some(());
            let (prefs, broken_prefs) = parse_config(&config_dir().join("ui.json"));
            app.core.setup().prefs = prefs;
            app.core.setup().prefs.migrate();
            app.core.setup().values = load_values();
            app.core.setup().prefs.appearance.apply(&cc.egui_ctx);
            // A hand-edited size that is not offered shows and applies as 100 %.
            app.core.setup().prefs.ui_scale = app.core.setup().prefs.look().scale;
            if app.core.setup().prefs.look() != Look::default() {
                style(&cc.egui_ctx, app.core.setup().prefs.look());
            }
            let alive: Vec<u64> = metas.iter().map(|m| m.id).collect();
            let (groups, broken_groups) = groups::load_groups();
            let library_existed = config_dir().join("library.json").exists();
            let (library, broken_library) = library::load_library();
            app.core.setup().groups = groups;
            app.core.setup().library = library;
            // The first run with a library: the queries saved in ui.json move over, once. ui.json keeps them as they were.
            // A library.json that could not be read at all was moved aside and left an empty library; the old queries are
            // still in ui.json, so they come over again rather than staying lost.
            if library::should_migrate(library_existed, broken_library.is_some(), &app.core.setup().library) {
                app.migrate_library();
            }
            // Connections removed since the last run leave their groups, but their scripts stay.
            if app.core.setup().groups.prune(&alive) {
                app.save_groups();
            }
            crate::vault::migrate(&secret_names(metas.iter().map(|m| m.id)));
            for meta in metas {
                let masked = stored_uri(meta.id).ok().map(|u| db::mask_uri(&u)).unwrap_or_default();
                app.core.setup().conns.push(Conn { masked_uri: masked, ..Conn::new(meta, Status::Idle) });
            }
            app.core.setup().active = app.core.setup().conns.first().map(|c| c.meta.id);
            // Values of deleted connections go with them.
            let ids: Vec<u64> = app.core.conns.iter().map(|c| c.meta.id).collect();
            app.core.setup().values.retain(|k, _| ids.iter().any(|id| k.starts_with(&format!("{id}/"))));
            // Reopen the tabs of the last session; they load once their connection is connected.
            for st in app.core.setup().prefs.open_tabs.clone() {
                if app.conn(st.conn).is_none() {
                    continue;
                }
                let engine = app.conn(st.conn).map(|c| c.meta.engine).unwrap_or_default();
                let mut t = Tab::new(Sel { conn: st.conn, db: st.db, schema: st.schema, coll: st.coll }).for_engine(engine);
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
                    // The saved tree is used once the columns arrive; until then it is saved again as it came.
                    s.bar.pending = std::mem::take(&mut s.bar.c.tree);
                    s.bar.tree_json = s.bar.pending.clone();
                }
                if let Some(s) = t.sql.as_mut() {
                    (s.query, s.run_db, s.run_schema) = (st.query, st.run_db, st.run_schema);
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
                // A tab opened from the library is bound to its script again, when the script is still there and the tab
                // still shows the view the script is written in. Its base is the saved text, so edits made before the
                // restart still show as edits.
                if let Some(r) = st.script {
                    t.script = app.rebind(&t, r);
                }
                t.seed_ran();
                app.core.setup().tabs.push(t);
            }
            app.core.setup().cur = app.core.setup().prefs.cur_tab.min(app.core.setup().tabs.len().saturating_sub(1));
            if let Some(id) = app.core.setup().prefs.last_conn.filter(|&id| app.conn(id).is_some()) {
                app.core.setup().active = Some(id);
            }
            // The active tab wins over the saved connection, so the sidebar starts on the tab's connection like after a tab switch.
            if let Some(conn) = app.core.tabs.get(app.core.cur).map(|t| t.sel.conn) {
                app.core.setup().active = Some(conn);
            }
            // Every connection open last time comes back open, in its rail order. The connections of reopened tabs
            // count too, for settings saved before open_conns existed.
            let mut seen = std::collections::HashSet::new();
            let open: Vec<u64> = app.core.prefs.open_conns.iter().copied().chain(app.core.active).chain(app.core.tabs.iter().map(|t| t.sel.conn)).filter(|id| app.conn(*id).is_some() && seen.insert(*id)).collect();
            app.core.setup().rail_order = open.clone();
            for id in open {
                app.connect(id);
            }
            app.core.setup().agents_start();
            // A fresh install has no ui.json, so it skips the notes. Run now, so the broken-settings note below still wins.
            app.core.setup().whats_new_seen(ui_json.is_some());
            app.run_fx();
            let broken: Vec<String> = [broken_conns, broken_prefs, broken_groups, broken_library].into_iter().flatten().collect();
            if !broken.is_empty() {
                app.modal = Some(Modal::Text { title: "Some settings could not be loaded".into(), body: broken.join("\n\n"), code: false });
            }
        }
        app
    }

    /// A menu pick that calls its command. A page command waits for the page, as the key it used to replay did.
    #[cfg(target_os = "macos")]
    fn menu_act(&mut self, ctx: &egui::Context, c: Cmd) {
        if matches!(c.spec().scope, quarry_core::cmd::Scope::Page | quarry_core::cmd::Scope::View) && (self.layer() != quarry_core::cmd::Layer::Page || !c.keys_on(&self.core, ctx.text_edit_focused())) {
            return;
        }
        self.run_cmd(c, ctx);
    }

    /// Keeps one scroll gesture on the axis it started on, so a slightly diagonal trackpad swipe
    /// does not drift off the row the eye follows. See `scroll_axis` for when a gesture locks.
    pub(super) fn lock_scroll_axis(&mut self, ctx: &egui::Context) {
        if !self.core.prefs.lock_scroll_axis {
            return;
        }
        let (d, started, t) = ctx.input(|i| (i.smooth_scroll_delta, i.events.iter().any(|e| matches!(e, egui::Event::MouseWheel { phase: egui::TouchPhase::Start, .. })), i.time));
        if let Some(vertical) = scroll_axis(&mut self.scroll_gesture, d, started, t) {
            ctx.input_mut(|i| if vertical { i.smooth_scroll_delta.x = 0.0 } else { i.smooth_scroll_delta.y = 0.0 });
        }
    }
}

/// Which axis a scroll frame keeps, or None to leave it alone. Like UIScrollView's directional lock:
/// a gesture that starts within about 27 degrees of an axis rides that axis, momentum included,
/// and a clearly diagonal start scrolls freely, because locking every gesture felt too restrictive.
/// A locked gesture lets go for good once the last few frames lean clearly toward the other axis,
/// so turning mid-swipe works without lifting the fingers.
/// The start is judged on the first 8 pt travelled, since single frames at the start of a swipe are noisy.
/// Fingers touching the trackpad start a new gesture (and stop momentum), so a swipe the other way
/// takes over at once. Wheels and platforms that send no phases start one after a 0.2 s pause.
fn scroll_axis(g: &mut (egui::Vec2, Option<Option<bool>>, f64, egui::Vec2), d: egui::Vec2, started: bool, t: f64) -> Option<bool> {
    if started || (d != egui::Vec2::ZERO && t - g.2 > 0.2) {
        *g = (egui::Vec2::ZERO, None, t, egui::Vec2::ZERO);
    }
    if d == egui::Vec2::ZERO {
        return None;
    }
    g.0 += d;
    g.2 = t;
    // ponytail: decays per frame, so the window is about 3 frames at any frame rate; a time constant if that ever matters.
    g.3 = g.3 * 0.7 + d;
    if let Some(Some(vertical)) = g.1 {
        let (on, off) = if vertical { (g.3.y.abs(), g.3.x.abs()) } else { (g.3.x.abs(), g.3.y.abs()) };
        // ponytail: feel knobs; 4 pt keeps end-of-swipe jitter from releasing, 0.8 is about 39 degrees of recent travel.
        if off >= 4.0 && off >= 0.8 * on {
            g.1 = Some(None);
        }
    }
    let (x, y) = (g.0.x.abs(), g.0.y.abs());
    if g.1.is_none() && g.0.length() >= 8.0 {
        // ponytail: the threshold is a feel knob; 0.5 is tan(27 degrees).
        g.1 = Some((x.min(y) <= 0.5 * x.max(y)).then_some(y >= x));
    }
    // Undecided frames keep the leading axis, so a near-axis swipe does not wobble before its lock.
    g.1.unwrap_or(Some(y >= x))
}

impl eframe::App for App {
    /// Test switch for the demo picture: QUARRY_TEST_POINTER="x,y" hovers that point,
    /// "x,y,down" also holds the primary button there, to check hover and press states, and
    /// "x,y,right" right-clicks there once on frame 60, so the capture shows the context menu;
    /// "x,y,right,x2,y2" then moves the pointer to x2,y2 to open a submenu; "x,y,click" left-clicks
    /// there once on frame 60, to open a menu button's popover.
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
        if let (Some("right"), Some(Ok(x2)), Some(Ok(y2))) = (mode, parts.next().map(str::parse::<f32>), parts.next().map(str::parse::<f32>)) && self.shot.as_ref().is_some_and(|s| s.1 > 62) {
            pos = egui::pos2(x2, y2);
        }
        raw.events.push(egui::Event::PointerMoved(pos));
        match mode {
            Some("down") => raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }),
            // One right-click partway through, so the capture shows the context menu settled.
            Some(m @ ("right" | "click")) if self.shot.as_ref().is_some_and(|s| s.1 == 60) => {
                let button = if m == "right" { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
                for pressed in [true, false] {
                    raw.events.push(egui::Event::PointerButton { pos, button, pressed, modifiers: Default::default() });
                }
            }
            _ => {}
        }
    }

    // Persistence is on only to keep the window size and position; restoring egui's widget state would change behavior.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Here rather than in ui(): eframe skips ui() while the window is minimised or covered, and agent calls
        // and other worker answers must still be handled then. logic() runs first, so the order is unchanged.
        self.poll(ctx);
        self.run_fx();
        if let Some(text) = self.clip.take() {
            ctx.copy_text(text);
        }
        // A minimised or hidden Quarry would otherwise let the approval time out unseen.
        if std::mem::take(&mut self.attention) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
        LIGHT_MODE.store(ctx.theme() == egui::Theme::Light, std::sync::atomic::Ordering::Relaxed);
        crate::ui::vim::set_on(ctx, self.core.prefs.vim);
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
        self.library_tick(ctx);
        self.core.flush_prefs();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.ui.frame_ms = frame.info().cpu_usage.unwrap_or(0.0) * 1000.0;
        // QUARRY_FRAME_LOG prints each frame's CPU time, for measuring with a fixture.
        if std::env::var("QUARRY_FRAME_LOG").is_ok() {
            eprintln!("frame_ms {:.3}", self.ui.frame_ms);
        }
        let ctx = ui.ctx().clone();
        self.lock_scroll_axis(&ctx);
        self.vim_begin(&ctx);
        self.record_key(&ctx);
        set_window_over_page(&ctx, self.window_open());
        // AppKit resets the window buttons on every layout, so re-centre them in our header each frame.
        frame.set_traffic_lights_position(&ctx, egui::Rangef::new(0.0, HEADER_H), 16.0);
        #[cfg(target_os = "linux")]
        if self.ui.win_buttons {
            window_buttons::resize_edges(&ctx);
        }
        self.top_bar(ui);
        self.status_bar(ui);
        let corner = self.rail(ui);
        if !self.core.prefs.sidebar_hidden {
            self.sidebar(ui);
        }
        vim_nav::side_done(&ctx);
        self.query_log(ui);
        if !self.core.prefs.open_inline && self.selected_doc().is_some() {
            self.inspector(ui);
        }
        self.value_panel(ui);
        // After the side panels, so the strip spans only the workspace.
        if !self.core.tabs.is_empty() {
            self.tab_strip(ui);
        }
        egui::CentralPanel::default_margins().frame(egui::Frame::new().fill(BG()).inner_margin(egui::Margin { left: 20, right: 20, top: 10, bottom: 12 })).show(ui, |ui| {
            #[cfg(test)]
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("test-central-left"), ui.max_rect().left() - 20.0));
            if self.core.tabs.is_empty() {
                self.welcome(ui);
            } else {
                self.collection(ui);
            }
        });
        self.page_corner(&ctx, corner);
        self.auto_update(&ctx);
        // Before the modals, so one opened by a shortcut draws this frame, and an Esc that closes a
        // modal is not also taken as a second Esc by `keys`.
        self.keys(&ctx);
        self.modals(&ctx);
        self.users_window(&ctx);
        self.sql_users_window(&ctx);
        self.pg_roles_window(&ctx);
        self.server_window(&ctx);
        self.sql_server_window(&ctx);
        self.pg_server_window(&ctx);
        self.routine_window(&ctx);
        self.tool_window(&ctx);
        // After the modals, so a browser opened from the connection form draws over it.
        self.cloud_browser_window(&ctx);
        // Opened from Settings, so after the modals to draw over them.
        self.agents_rules_window(&ctx);
        self.agents_activity_window(&ctx);
        self.toasts(&ctx);
        // Last: an agent call waiting for an answer stacks above everything, Settings included.
        self.agents_approval(&ctx);
        self.screenshot(&ctx);
        self.vim_end(&ctx);
        // Every setting changed this frame goes to ui.json in one write.
        self.core.flush_prefs();
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

pub(crate) use crate::db::group;

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
        assert_eq!(query_script("default", "", ""), "async function run() {\n\treturn await db.collection(\"default\").find({})\n}");
    }

    #[test]
    fn scroll_axis_locks_near_axis_gestures_and_frees_diagonal_ones() {
        let v = egui::vec2;
        let mut g = Default::default();
        // A sideways first frame does not decide a mostly vertical swipe.
        assert_eq!(scroll_axis(&mut g, v(2.0, 0.5), true, 0.0), Some(false));
        assert_eq!(scroll_axis(&mut g, v(1.0, 8.0), false, 0.016), Some(true));
        assert_eq!(g.1, Some(Some(true)));
        // Once locked, a slight drift and the momentum after it stay vertical.
        assert_eq!(scroll_axis(&mut g, v(2.0, 8.0), false, 0.032), Some(true));
        assert_eq!(scroll_axis(&mut g, egui::Vec2::ZERO, false, 0.048), None);
        // A small sideways frame does not let go, turning clearly sideways does, for the rest of the gesture.
        assert_eq!(scroll_axis(&mut g, v(3.0, 1.0), false, 0.064), Some(true));
        assert_eq!(scroll_axis(&mut g, v(9.0, 1.0), false, 0.08), None);
        assert_eq!(scroll_axis(&mut g, v(0.0, 9.0), false, 0.096), None);
        // Fingers down start a new gesture, which can go across right away.
        assert_eq!(scroll_axis(&mut g, v(10.0, 1.0), true, 0.06), Some(false));
        // A diagonal start scrolls freely for the whole gesture.
        assert_eq!(scroll_axis(&mut g, v(7.0, 6.0), true, 0.1), None);
        assert_eq!(scroll_axis(&mut g, v(0.0, 9.0), false, 0.116), None);
        // A wheel with no phases starts a new gesture after a pause.
        assert_eq!(scroll_axis(&mut g, v(0.0, 12.0), false, 0.5), Some(true));
    }

}
