//! App methods over the state types, which live in quarry_core::state.

use super::*;
pub(crate) use quarry_core::state::*;

/// What to run once writes are allowed.
pub(super) type Retry = Box<dyn FnOnce(&mut App)>;

/// The core's dialogs, carrying the app closure an unlock reruns.
pub(crate) type Modal = quarry_core::state::Modal<Retry>;

/// What only drawing cares about: it lives in the app, not in Core, so a frame can change it without an action.
pub(crate) struct UiState {
    /// Scroll the selected document row into view on its next draw, after keyboard selection.
    pub scroll_sel: bool,
    /// The active tab the tab strip last scrolled to; it scrolls again when this differs.
    pub tab_seen: Option<usize>,
    /// Whether the header draws the window buttons: on Windows once the native title bar is gone, and in the demo's
    /// QUARRY_TEST_WINFRAME switch.
    pub win_buttons: bool,
    /// CPU time of the last frame in milliseconds, for the frame rate readout.
    pub frame_ms: f32,
    /// Last memory and CPU sample for the status bar: when, total CPU time then, CPU percent, bytes.
    pub usage: (std::time::Instant, Duration, f32, Option<u64>),
    /// Frame rate readout: seconds and frames counted so far, and the rate last shown.
    pub fps: (f32, u32, f32),
    /// Opens the error list on its first frame, for the demo picture.
    pub errors_open: bool,
    /// Opens the update popover on the next frame, after a check started from the menu or the palette.
    pub upd_popup: bool,
    /// The query log panel: open, detached, its filter and the errors-only switch.
    pub log: LogUi,
    /// Cmd+O asked for the quick picker of the view; the view that draws one clears it.
    pub open_picker: bool,
    /// Cmd+Shift+L asked for the library tree: the panel selects its first row if none is selected and takes the keys.
    pub lib_focus_tree: bool,
    /// What the Connections panel and the rail's Connect popover remember: filters, the group being named, demo states.
    pub conns: quarry_core::state::conns_panel::ConnsUi,
    /// Alignment, widths, pins, hidden columns and highlights of Query results, by the column names of a result, for each SQL
    /// tab by uid. Kept for the tab only: ad hoc results have no table to save them under.
    pub qprefs: HashMap<u64, HashMap<Vec<String>, ColPrefs>>,
    /// The selected row of each grid of the Structure views, by tab uid: columns, indexes, keys out, keys in, triggers, checks.
    pub struct_sel: HashMap<u64, [Option<usize>; 6]>,
    /// The statement and warnings of a tab's staged changes, kept between frames.
    pub plan_memo: HashMap<u64, crate::app::sql::alter::PlanMemo>,
    /// What the trigger editor shows for a tab's draft, kept between frames.
    pub trig_memo: HashMap<u64, crate::app::sql::trig::TrigMemo>,
    /// The suggestion list of each Query tab's script editor, by tab uid.
    pub sql_ac: HashMap<u64, quarry_core::state::complete::Ac>,
    /// What the Content bar of each SQL tab keeps between frames, by tab uid.
    pub bar_ui: HashMap<u64, crate::app::sql::bar::BarUi>,
}

impl Default for UiState {
    fn default() -> Self {
        Self { scroll_sel: false, tab_seen: None, win_buttons: false, frame_ms: 0.0, usage: (std::time::Instant::now(), Duration::ZERO, 0.0, None), fps: (0.0, 0, 0.0), errors_open: false, upd_popup: false, log: LogUi::default(), open_picker: false, lib_focus_tree: false, conns: Default::default(), qprefs: HashMap::new(), struct_sel: HashMap::new(), plan_memo: HashMap::new(), trig_memo: HashMap::new(), sql_ac: HashMap::new(), bar_ui: HashMap::new() }
    }
}

impl App {
    pub(super) fn ok(&mut self, s: impl Into<String>) {
        self.core.ok(s)
    }

    pub(super) fn conn(&self, id: u64) -> Option<&Conn> {
        self.core.conn(id)
    }

    pub(super) fn t(&self) -> Option<&Tab> {
        self.core.tabs.get(self.core.cur)
    }

    pub(super) fn sel_is_view(&self) -> bool {
        self.core.sel_is_view()
    }

    pub(super) fn sel_writable(&self) -> bool {
        self.core.sel_writable()
    }

    pub(super) fn timeout(&self) -> Duration {
        self.core.timeout()
    }

    pub(super) fn target(&self) -> Option<Target> {
        self.core.target()
    }

    pub(super) fn writable_target(&mut self) -> Option<(u64, Target)> {
        self.core.writable_target()
    }

    /// Asks to allow writes, then runs `then` if they were allowed.
    pub(super) fn ask_unlock_then(&mut self, then: impl FnOnce(&mut App) + 'static) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, Some(Box::new(then))));
        }
    }

    pub(super) fn connect(&mut self, id: u64) {
        self.exec(Action::Conn(ConnA::Connect { conn: id }));
    }

    /// Something a restart would lose: unsaved edits, writes in flight, running scripts or tools.
    /// Work a restart would lose. Settings does not count, since Restart sits in it; every other dialog may hold typed input.
    pub(super) fn busy_work(&self) -> bool {
        self.modal.as_ref().is_some_and(|m| !matches!(m, Modal::Settings)) || self.core.work_held()
    }
}
