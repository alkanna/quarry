//! Tabs: opening, closing and reopening them.

use super::*;

impl App {
    pub(super) fn col_key(&self, agg: bool) -> String {
        let s = self.t().map(Tab::coll_key).unwrap_or_default();
        match self.t().filter(|_| agg) {
            Some(t) => format!("{s}#{}", t.agg_source().3),
            None => s,
        }
    }

    pub(super) fn in_coll(&self) -> bool {
        self.t().is_none_or(|t| t.in_coll())
    }

    /// Closes open rows of the result table that is not the Documents one, after its rows changed under them.
    pub(super) fn drop_agg_row(&mut self) {
        let Some(t) = self.tm() else { return };
        t.expanded.retain(|k| !k.0);
        if t.selected.is_some_and(|s| s.0) {
            t.selected = None;
            self.reset_doc_state();
        }
    }

    pub(super) fn reset_doc_state(&mut self) {
        self.json_edit = None;
        self.inline = None;
        self.new_field = None;
    }

    /// Opens a collection. Like the original (treeOpenInNewTab), a new tab is used unless one
    /// already shows that collection; `replace` reuses the current tab instead.
    pub(super) fn open_collection(&mut self, sel: Sel, replace: bool) {
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
    pub(super) fn remember_closed(&mut self, t: Tab) {
        self.closed.insert(0, t);
        self.closed.truncate(10);
    }

    pub(super) fn reopen_tab(&mut self) {
        if self.closed.is_empty() {
            return;
        }
        let t = self.closed.remove(0);
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
        self.reset_doc_state();
        self.reload();
    }

    pub(super) fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let mut t = self.tabs.remove(i);
        // A closed tab is kept for reopening; its stream must not keep reading meanwhile.
        t.watch.stop();
        self.remember_closed(t);
        if self.cur > i || self.cur >= self.tabs.len() {
            self.cur = self.cur.saturating_sub(1);
        }
        self.reset_doc_state();
    }

    pub(super) fn tab_by_req(&mut self, req: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.req == req)
    }
}
