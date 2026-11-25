//! Tabs: what the app keeps of them. Opening, closing and reopening run in Core (`quarry_core::action::tabs`); these
//! forward for the other areas' call sites.

use super::*;
use quarry_core::action::tabs::tab_for;

impl App {
    pub(super) fn col_key(&self, agg: bool) -> String {
        self.core.col_key(agg)
    }

    pub(super) fn in_coll(&self) -> bool {
        self.core.in_coll()
    }

    pub(super) fn drop_agg_row(&mut self) {
        self.core.drop_agg_row()
    }

    pub(super) fn reset_doc_state(&mut self) {
        self.core.reset_doc_state()
    }

    pub(super) fn set_cur(&mut self, i: usize) {
        self.core.set_cur(i)
    }

    pub(super) fn nav(&mut self, back: bool) {
        self.core.nav(back)
    }

    pub(super) fn open_collection(&mut self, sel: Sel, how: Open) {
        self.core.open_collection(sel, how)
    }

    pub(super) fn open_tab(&mut self, sel: Sel, how: Open) -> bool {
        self.core.open_tab(sel, how)
    }

    pub(super) fn reopen_tab(&mut self) {
        self.core.reopen_tab()
    }

    pub(super) fn close_tab(&mut self, i: usize) {
        self.core.close_tab(i)
    }

    pub(super) fn remove_tabs(&mut self, gone: impl Fn(&Tab) -> bool) {
        self.core.remove_tabs(gone)
    }

    pub(super) fn go_tab(&mut self, i: usize) {
        self.core.go_tab(i)
    }

    pub(super) fn set_view(&mut self, v: View) {
        self.core.set_view(v)
    }

    pub(super) fn reload(&mut self) {
        self.core.reload()
    }

    pub(super) fn reload_of(&mut self, i: usize) {
        self.core.reload_of(i)
    }

    /// Runs `f` with the tab a confirmation was raised for as the current one, then puts the current tab back. A
    /// tab can close or move while the dialog is open, so the action is refused rather than run on another table.
    pub(super) fn on_tab(&mut self, sel: &Sel, f: impl FnOnce(&mut App)) {
        let Some(i) = tab_for(self.core.tabs.iter().map(|t| &t.sel), self.core.cur, sel) else {
            return self.warn(format!("Nothing ran: {} is no longer open in a tab.", sel.coll));
        };
        let back = self.core.cur;
        self.core.cur = i;
        f(self);
        if back < self.core.tabs.len() {
            self.core.cur = back;
        }
    }

    pub(super) fn tab_by_req(&mut self, req: u64) -> Option<&mut Tab> {
        self.core.tab_by_req(req)
    }
}

/// Cmd+click and middle-click open a new tab, as links do in a browser.
pub(super) fn new_tab_click(r: &egui::Response) -> bool {
    r.middle_clicked() || r.clicked() && r.ctx.input(|i| i.modifiers.command)
}
