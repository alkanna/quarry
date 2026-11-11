//! Headless tests that drive the real `App` through egui_kittest, against live servers.
//!
//! `ui_app` returns None when its environment variable is unset, so a test returns early like the live
//! worker tests. The app runs with `ephemeral` set: no files, no keychain, no menu bar, no window.
//! Widgets are found by their accessible names (`Response::widget_info`): "{col} row {n}" for a grid
//! cell, "Select row {n}" for its checkbox, the column name for a header, the text of a sidebar row.

mod conn_advanced;
mod mongo_edit;
mod sidebar;
mod sql_dump;
mod sql_edit;
mod sql_import;
mod sql_relations;
mod sql_structure;
mod sql_triggers;

use super::*;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

const LIMIT: Duration = Duration::from_secs(20);
const CONN: u64 = 1;

/// Which of the widgets with one accessible name a click is for.
#[derive(Clone, Copy)]
enum Pick {
    Only,
    Last,
    Nth(usize),
}

pub(super) struct UiApp {
    pub h: Harness<'static, App>,
}

/// A harness with the server of `var` added as the only connection, connecting. None when `var` is unset.
pub(super) fn ui_app(var: &str) -> Option<UiApp> {
    let uri = std::env::var(var).ok()?;
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let app = h.state_mut();
    let engine = db::sql::Engine::from_uri(&uri).unwrap_or_default();
    app.conns.push(Conn { masked_uri: db::mask_uri(&uri), ..Conn::new(ConnMeta { id: CONN, name: "test".into(), engine, ..Default::default() }, Status::Idle) });
    app.active = Some(CONN);
    app.rail_order.push(CONN);
    app.connect_uri(CONN, uri);
    let mut ui = UiApp { h };
    ui.wait("the connection to be ready", |a| a.conn(CONN).is_some_and(|c| c.status == Status::Ready));
    Some(ui)
}

impl UiApp {
    pub fn app(&self) -> &App {
        self.h.state()
    }

    /// Steps frames until `cond` holds. Spinners keep asking for repaints, so `run()` never settles and is not used.
    pub fn wait(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        let t = std::time::Instant::now();
        loop {
            self.h.step();
            if cond(self.h.state()) {
                return;
            }
            assert!(t.elapsed() < LIMIT, "timed out waiting for {what}; last error: {}", self.app().last_problem());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Frames for queued input and the answers it triggers to land.
    pub fn settle(&mut self) {
        self.h.run_steps(4);
    }

    /// The widget with this accessible name that `pick` names.
    fn node<'a>(&'a self, label: &'a str, pick: Pick) -> egui_kittest::Node<'a> {
        match pick {
            Pick::Only => self.h.get_by_label(label),
            Pick::Last => self.h.get_all_by_label(label).last().expect("a widget"),
            Pick::Nth(n) => self.h.get_all_by_label(label).nth(n).expect("a widget"),
        }
    }

    /// Clicks a widget after scrolling it into view. A list longer than its popup or window keeps an item outside the area that
    /// is drawn, and egui drops a pointer click there, so a click on a far item of a picker would do nothing.
    fn click_pick(&mut self, label: &str, pick: Pick) {
        self.show(label);
        // The scroll area takes the offset on one frame and lays the items out at it on the next, and the node's rect is read from
        // the frame before the click. A form that has just opened lays out its content over several frames and a first scroll can
        // land short, so it scrolls again until the item stops moving.
        let mut at = self.node(label, pick).rect();
        let mut settled = false;
        for _ in 0..5 {
            self.node(label, pick).scroll_to_me();
            self.h.run_steps(3);
            let now = self.node(label, pick).rect();
            if now == at {
                settled = true;
                break;
            }
            at = now;
        }
        assert!(settled, "\"{label}\" kept moving after 5 scrolls, so a click on it would land somewhere else");
        self.node(label, pick).click();
        self.settle();
    }

    /// Waits for a widget with this exact accessible name, then clicks it.
    pub fn click(&mut self, label: &str) {
        self.click_pick(label, Pick::Only);
    }

    /// Waits for a widget whose accessible name contains `part`, then clicks it. For a menu item whose name carries more
    /// than the test knows, such as a column with its type.
    pub fn click_contains(&mut self, part: &str) {
        let t = std::time::Instant::now();
        while self.h.query_all_by_label_contains(part).next().is_none() {
            assert!(t.elapsed() < LIMIT, "timed out waiting for a widget with \"{part}\" on screen; last error: {}", self.app().last_problem());
            self.h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.h.get_by_label_contains(part).click();
        self.settle();
    }

    /// Waits until a widget with this accessible name is on screen.
    pub fn show(&mut self, label: &str) {
        let t = std::time::Instant::now();
        while self.h.query_all_by_label(label).next().is_none() {
            assert!(t.elapsed() < LIMIT, "timed out waiting for \"{label}\" on screen; last error: {}", self.app().last_problem());
            self.h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Opens `table` from the sidebar: expands the database, picks the schema on PostgreSQL, clicks the table, and waits for its rows.
    pub fn open_table(&mut self, db: &str, schema: &str, table: &str) {
        let pg = self.app().conn(CONN).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres);
        if !self.app().open_dbs.contains(&(CONN, db.to_string())) {
            self.click(db);
        }
        if pg {
            self.wait("the schema list", |a| a.conn(CONN).is_some_and(|c| c.pg.contains_key(db)));
            let picked = self.app().conn(CONN).and_then(|c| c.pg.get(db)).map(|p| p.picked.clone()).unwrap_or_default();
            if picked != schema {
                self.click(&format!("Schema {picked}"));
                self.click(schema);
            }
        }
        self.click(table);
        self.wait("the table rows", |a| a.t().is_some_and(|t| t.sel.coll == table && !t.loading && !t.docs.is_empty()));
    }

    /// Allows writes on the connection through the READ-ONLY badge and its dialog.
    pub fn unlock(&mut self) {
        self.click("READ-ONLY");
        self.click("Allow writes");
        self.wait("writes to be allowed", |a| a.conn(CONN).is_some_and(|c| c.writable));
    }

    /// The grid cell of a column in a row, numbered as the grid shows it (from 1).
    pub fn cell(&self, col: &str, row: usize) -> egui_kittest::Node<'_> {
        // The node borrows its query text, so the short label is leaked to give it the harness lifetime.
        self.h.get_by_label(Box::leak(format!("{col} row {row}").into_boxed_str()))
    }

    /// Double-clicks a cell. Both clicks go into one frame: a kittest step lasts 250 ms of egui time, longer than egui's double click delay.
    pub fn double_click_cell(&mut self, col: &str, row: usize) {
        self.show(&format!("{col} row {row}"));
        let at = self.cell(col, row).rect().center();
        self.h.hover_at(at);
        self.h.step();
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        self.h.input_mut().events.extend([press(true), press(false), press(true), press(false)]);
        self.h.step();
    }

    /// Double-clicks a cell and waits for its editor, which replaces the cell and is focused with its text selected.
    pub fn open_cell(&mut self, col: &str, row: usize) {
        self.double_click_cell(col, row);
        self.wait("the cell editor", |a| a.sql_cell.is_some());
    }

    /// Types into the focused widget.
    pub fn type_text(&mut self, text: &str) {
        self.h.event(egui::Event::Text(text.into()));
        self.settle();
    }

    /// Opens a cell, types over its text and commits with Enter.
    pub fn edit_cell(&mut self, col: &str, row: usize, text: &str) {
        self.open_cell(col, row);
        self.type_text(text);
        self.h.key_press(egui::Key::Enter);
        self.wait("the edit to be staged", |a| a.sql_cell.is_none());
    }

    /// Clicks the last widget with this accessible name: a dialog is drawn after the page, so its button comes
    /// last when the page has one of the same name ("Delete").
    pub fn click_last(&mut self, label: &str) {
        self.click_pick(label, Pick::Last);
    }

    /// Clicks the nth widget with this accessible name.
    pub fn click_nth(&mut self, label: &str, n: usize) {
        self.click_pick(label, Pick::Nth(n));
    }

    /// Clicks a text field by its accessible name and types into it.
    pub fn fill(&mut self, field: &str, text: &str) {
        self.click(field);
        self.type_text(text);
    }

    /// Clicks Save in the staged bar and waits until nothing is staged or in flight.
    pub fn save(&mut self) {
        self.click("Save");
        self.wait("the save to finish", |a| a.saving.is_empty() && a.drafts.is_empty());
    }
}
