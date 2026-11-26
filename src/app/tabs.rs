//! Tabs: what the app keeps of them. Opening, closing and reopening run in Core (`quarry_core::action::tabs`); these
//! push the action for the other areas' call sites. A tab is named by its uid, because an index moves when tabs close.

use super::*;
use quarry_core::action::tabs::tab_for;
use quarry_core::action::{Action, TabA};

impl App {
    pub(super) fn col_key(&self, agg: bool) -> String {
        self.core.col_key(agg)
    }

    pub(super) fn in_coll(&self) -> bool {
        self.core.in_coll()
    }

    pub(super) fn drop_agg_row(&mut self) {
        self.exec(Action::Tab(TabA::DropAggRow));
    }

    pub(super) fn reset_doc_state(&mut self) {
        self.exec(Action::Tab(TabA::ResetDoc));
    }

    pub(super) fn uid_of(&self, i: usize) -> Option<u64> {
        self.core.tabs.get(i).map(|t| t.uid)
    }

    pub(super) fn set_cur(&mut self, i: usize) {
        if let Some(tab) = self.uid_of(i) {
            self.exec(Action::Tab(TabA::Select { tab }));
        }
    }

    pub(super) fn open_collection(&mut self, sel: Sel, how: Open) {
        self.exec(Action::Tab(TabA::Open { sel, how }));
    }

    pub(super) fn open_tab(&mut self, sel: Sel, how: Open) {
        self.exec(Action::Tab(TabA::OpenTab { sel, how }));
    }

    pub(super) fn close_tab(&mut self, i: usize) {
        if let Some(tab) = self.uid_of(i) {
            self.exec(Action::Tab(TabA::Close { tab }));
        }
    }

    /// Closes the tabs `gone` picks. The app reads the tabs to choose; closing them is the action's.
    pub(super) fn remove_tabs(&mut self, gone: impl Fn(&Tab) -> bool) {
        let tabs: Vec<u64> = self.core.tabs.iter().filter(|t| gone(t)).map(|t| t.uid).collect();
        if !tabs.is_empty() {
            self.exec(Action::Tab(TabA::CloseTabs { tabs }));
        }
    }

    pub(super) fn go_tab(&mut self, i: usize) {
        if let Some(tab) = self.uid_of(i) {
            self.exec(Action::Tab(TabA::Pick { tab }));
        }
    }

    pub(super) fn set_view(&mut self, view: View) {
        if let Some(tab) = self.uid_of(self.core.cur) {
            self.exec(Action::Tab(TabA::View { tab, view }));
        }
    }

    pub(super) fn reload(&mut self) {
        self.reload_of(self.core.cur);
    }

    pub(super) fn reload_of(&mut self, i: usize) {
        if let Some(tab) = self.uid_of(i) {
            self.exec(Action::Tab(TabA::Reload { tab }));
        }
    }

    /// Runs `f` with the tab a confirmation was raised for as the current one, then puts the current tab back. A
    /// tab can close or move while the dialog is open, so the action is refused rather than run on another table.
    pub(super) fn on_tab(&mut self, sel: &Sel, f: impl FnOnce(&mut App)) {
        let Some(i) = tab_for(self.core.tabs.iter().map(|t| &t.sel), self.core.cur, sel) else {
            return self.warn(format!("Nothing ran: {} is no longer open in a tab.", sel.coll));
        };
        let back = self.uid_of(self.core.cur);
        if let Some(tab) = self.uid_of(i) {
            self.exec(Action::Tab(TabA::Focus { tab }));
        }
        f(self);
        if let Some(tab) = back.filter(|b| self.core.tabs.iter().any(|t| t.uid == *b)) {
            self.exec(Action::Tab(TabA::Focus { tab }));
        }
    }
}

/// Cmd+click and middle-click open a new tab, as links do in a browser.
pub(super) fn new_tab_click(r: &egui::Response) -> bool {
    r.middle_clicked() || r.clicked() && r.ctx.input(|i| i.modifiers.command)
}
