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

    /// The one place the current tab changes. The explorer follows the tab's connection and scrolls
    /// to its database and collection. Picking a connection without changing tabs never comes here,
    /// so the explorer may show another connection than the tab.
    pub(super) fn set_cur(&mut self, i: usize) {
        if i != self.cur
            && let Some(prev) = self.tabs.get(self.cur).map(|t| t.uid)
        {
            if self.nav_back.last() != Some(&prev) {
                self.nav_back.push(prev);
            }
            // ponytail: 50 steps of history, plenty for tab hopping.
            if self.nav_back.len() > 50 {
                self.nav_back.remove(0);
            }
            self.nav_fwd.clear();
        }
        self.show_tab(i);
    }

    /// Makes tab `i` current without touching the back and forward history.
    fn show_tab(&mut self, i: usize) {
        // Its row key means nothing in another tab's rows, and a field edit would be staged against the other tab's collection.
        self.sql_cell = None;
        self.json_edit = None;
        self.cur = i;
        let Some(sel) = self.tabs.get(i).map(|t| t.sel.clone()) else { return };
        if let Some(id) = follow_conn(&sel, |id| self.conn(id).is_some()) {
            self.active = Some(id);
            if !sel.db.is_empty() {
                self.open_dbs.insert((id, sel.db.clone()));
                // The tree lists the schema of the tab, or its table would not be there to reveal.
                if !sel.schema.is_empty() && self.is_pg(id) {
                    self.pg_set_schema(id, &sel.db, sel.schema.clone());
                }
                self.reveal = Some((id, sel.db, (!sel.coll.is_empty()).then_some(sel.coll)));
            }
        }
    }

    /// Cmd+[ and Cmd+]: the previously shown tab, or the one gone back from. Tabs closed since are skipped.
    pub(super) fn nav(&mut self, back: bool) {
        let Some(cur) = self.t().map(|t| t.uid) else { return };
        let (from, to) = if back { (&mut self.nav_back, &mut self.nav_fwd) } else { (&mut self.nav_fwd, &mut self.nav_back) };
        if let Some(i) = nav_target(from, &self.tabs, self.cur) {
            to.push(cur);
            self.show_tab(i);
            self.reset_doc_state();
        }
    }

    /// Opens a collection, as a browser opens a page. `Here` goes to a tab already showing it, else takes the
    /// current tab when nothing would be lost there, else opens a tab next to it.
    pub(super) fn open_collection(&mut self, sel: Sel, how: Open) {
        // Before reset_doc_state, which closes the document editor this checks for.
        let editing = self.json_edit.is_some() || self.sql_cell.is_some();
        let keep = self.t().is_none_or(|t| editing || keeps_work(t, &self.drafts));
        self.reset_doc_state();
        if how == Open::Here
            && let Some(i) = self.tabs.iter().position(|t| t.sel == sel)
        {
            self.set_cur(i);
            return;
        }
        let engine = self.conn(sel.conn).map(|c| c.meta.engine).unwrap_or_default();
        let replace = !self.tabs.is_empty() && (how == Open::Replace || how == Open::Here && !keep);
        if replace {
            let old = &self.tabs[self.cur];
            // Content, Structure and the like carry over to the next table of the same kind of engine.
            let view = if how == Open::Replace || old.sql.is_some() == engine.is_sql() { old.view } else { View::Documents };
            let new = Tab { view, ..Tab::new(sel).for_engine(engine) };
            let mut old = std::mem::replace(&mut self.tabs[self.cur], new);
            if how == Open::Here {
                // Cmd+Shift+T brings the replaced tab back, as it does a closed one.
                old.watch.stop();
                self.remember_closed(old);
            }
            let i = self.cur;
            self.show_tab(i);
        } else {
            let at = if self.tabs.is_empty() { 0 } else { self.cur + 1 };
            self.tabs.insert(at, Tab::new(sel).for_engine(engine));
            self.set_cur(at);
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
        self.set_cur(self.tabs.len() - 1);
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
        let was_cur = self.cur == i;
        if self.cur > i || self.cur >= self.tabs.len() {
            self.cur = self.cur.saturating_sub(1);
        }
        // The tab that took the closed one's place is now current.
        if was_cur {
            self.set_cur(self.cur);
        }
        self.reset_doc_state();
    }

    /// Removes every tab `gone` picks. The current tab stays current when it is kept, wherever it moves to; when it
    /// goes, the tab that takes its place becomes current, as with `close_tab`.
    pub(super) fn remove_tabs(&mut self, gone: impl Fn(&Tab) -> bool) {
        let marks: Vec<bool> = self.tabs.iter().map(&gone).collect();
        if !marks.contains(&true) {
            return;
        }
        let was_cur = marks.get(self.cur).copied().unwrap_or(false);
        self.cur = cur_after(&marks, self.cur);
        let mut i = 0;
        self.tabs.retain(|_| {
            i += 1;
            !marks[i - 1]
        });
        if was_cur {
            self.set_cur(self.cur);
            self.reset_doc_state();
        }
    }

    /// Runs `f` with the tab a confirmation was raised for as the current one, then puts the current tab back. A
    /// tab can close or move while the dialog is open, so the action is refused rather than run on another table.
    pub(super) fn on_tab(&mut self, sel: &Sel, f: impl FnOnce(&mut App)) {
        let Some(i) = tab_for(self.tabs.iter().map(|t| &t.sel), self.cur, sel) else {
            return self.warn(format!("Nothing ran: {} is no longer open in a tab.", sel.coll));
        };
        let back = self.cur;
        self.cur = i;
        f(self);
        if back < self.tabs.len() {
            self.cur = back;
        }
    }

    pub(super) fn tab_by_req(&mut self, req: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.req == req)
    }
}

/// The index of the current tab once the tabs marked in `gone` are removed: the same tab when it is kept, else the
/// first kept tab after it, else the last one.
fn cur_after(gone: &[bool], cur: usize) -> usize {
    let left = gone.iter().filter(|g| !**g).count();
    let before = gone.iter().take(cur).filter(|g| **g).count();
    cur.saturating_sub(before).min(left.saturating_sub(1))
}

/// The tab showing `sel`: the current one when it does, else the first that does.
fn tab_for<'a>(sels: impl Iterator<Item = &'a Sel>, cur: usize, sel: &Sel) -> Option<usize> {
    let hits: Vec<usize> = sels.enumerate().filter(|(_, s)| *s == sel).map(|(i, _)| i).collect();
    hits.iter().copied().find(|&i| i == cur).or(hits.first().copied())
}

/// The connection the explorer should show for a tab, unless the tab's connection was removed.
fn follow_conn(sel: &Sel, exists: impl Fn(u64) -> bool) -> Option<u64> {
    exists(sel.conn).then_some(sel.conn)
}

/// Pops `from` until a tab still open other than `cur`, and returns its index.
fn nav_target(from: &mut Vec<u64>, tabs: &[Tab], cur: usize) -> Option<usize> {
    while let Some(uid) = from.pop() {
        if let Some(i) = tabs.iter().position(|t| t.uid == uid).filter(|&i| i != cur) {
            return Some(i);
        }
    }
    None
}

/// Whether a tab holds work a click elsewhere must not throw away: a workspace view, a query of the person's, or
/// staged edits. Such a tab is never reused by a plain click.
fn keeps_work(t: &Tab, drafts: &HashMap<String, Draft>) -> bool {
    matches!(t.view, View::Shell | View::Sql | View::Import | View::Compare | View::Watch)
        || has_clauses(t)
        || !t.proj_txt.trim().is_empty()
        || t.view == View::Aggregate && !t.agg_txt.trim().is_empty()
        || !drafts_of(drafts, &t.sel).is_empty()
}

/// Whether a tab holds a filter, sort or bar clause of the person's, which opening a row there would replace.
pub(super) fn has_clauses(t: &Tab) -> bool {
    let bar = t.sql.as_ref().is_some_and(|s| {
        let c = &s.bar.c;
        !(c.select.is_empty() && c.group.is_empty() && c.having.is_empty() && c.limit.is_empty() && c.offset.is_empty() && c.hint.is_empty() && !c.distinct)
    });
    !t.filter_txt.trim().is_empty() || !t.sort_txt.trim().is_empty() || bar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_reuses_only_a_tab_without_work_in_it() {
        let none = HashMap::new();
        assert!(!keeps_work(&Tab::new(sel("a")), &none), "a fresh tab is reused");
        let with = |f: fn(&mut Tab)| {
            let mut t = Tab::new(sel("a"));
            f(&mut t);
            keeps_work(&t, &none)
        };
        assert!(with(|t| t.filter_txt = "{a: 1}".into()), "a filter");
        assert!(with(|t| t.proj_txt = "{a: 1}".into()), "a projection");
        assert!(with(|t| t.view = View::Shell), "the Shell");
        assert!(with(|t| t.view = View::Watch), "a running change stream");
        assert!(!with(|t| t.view = View::Indexes), "a view of the collection itself");
        let mut drafts = HashMap::new();
        let s = sel("a");
        drafts.insert("k".into(), Draft { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone(), original: Document::new(), current: Document::new() });
        assert!(keeps_work(&Tab::new(sel("a")), &drafts), "staged edits");
        assert!(!keeps_work(&Tab::new(sel("b")), &drafts), "edits staged in another collection");
    }

    #[test]
    fn back_skips_closed_and_current_tabs() {
        // Two Query tabs on one database share a Sel; history must still tell them apart.
        let tabs: Vec<Tab> = ["", "", "c"].map(|c| Tab::new(sel(c))).into();
        let gone = Tab::new(sel("gone")).uid;
        // History 1, gone, c with c (index 2) current: back skips c itself and the closed tab.
        let mut back = vec![tabs[1].uid, gone, tabs[2].uid];
        assert_eq!(nav_target(&mut back, &tabs, 2), Some(1));
        assert!(back.is_empty());
        assert_eq!(nav_target(&mut back, &tabs, 2), None);
    }

    #[test]
    fn explorer_follows_the_tab_connection() {
        let sel = Sel { conn: 2, db: "shop".into(), coll: "orders".into(), ..Default::default() };
        assert_eq!(follow_conn(&sel, |id| id == 2), Some(2));
        assert_eq!(follow_conn(&sel, |id| id == 1), None);
    }

    fn sel(coll: &str) -> Sel {
        Sel { conn: 1, db: "shop".into(), coll: coll.into(), ..Default::default() }
    }

    /// Final review finding 3: a dropped table's tab before the current one used to leave `cur` on the next tab, so a
    /// confirmation still open ran on another table.
    #[test]
    fn removing_a_tab_before_the_current_one_keeps_the_current_tab() {
        // a, b, c, d with b current; a is dropped. Clamping kept index 1, which is now c.
        let tabs = [sel("a"), sel("b"), sel("c"), sel("d")];
        let gone: Vec<bool> = tabs.iter().map(|s| s.coll == "a").collect();
        let left: Vec<&Sel> = tabs.iter().zip(&gone).filter(|(_, g)| !**g).map(|(s, _)| s).collect();
        assert_eq!(left[cur_after(&gone, 1)].coll, "b");
        assert_eq!(cur_after(&gone, 1).min(left.len() - 1), 0);
        // The current tab itself goes: the next one takes its place, or the last one at the end.
        assert_eq!(cur_after(&[false, true, false], 1), 1);
        assert_eq!(cur_after(&[false, false, true], 2), 1);
        assert_eq!(cur_after(&[true, true], 1), 0);
        // Several before it, one after it.
        assert_eq!(cur_after(&[true, false, true, false, true], 3), 1);
    }

    /// Final review finding 3: Delete rows, Apply and Drop trigger carry the table they were confirmed for and run on
    /// the tab showing it, or not at all.
    #[test]
    fn a_confirmation_finds_its_table_after_the_tabs_moved() {
        let want = sel("b");
        // b moved from index 1 to 0 when a closed, and the current index now points at c.
        let after = [sel("b"), sel("c")];
        assert_eq!(tab_for(after.iter(), 1, &want), Some(0));
        // The current tab wins when two tabs show the table.
        let two = [sel("b"), sel("c"), sel("b")];
        assert_eq!(tab_for(two.iter(), 2, &want), Some(2));
        assert_eq!(tab_for(two.iter(), 1, &want), Some(0));
        // b itself was dropped: refused.
        assert_eq!(tab_for([sel("c")].iter(), 0, &want), None);
        // Another connection or database with the same table name is not the table.
        assert_eq!(tab_for([Sel { conn: 2, ..sel("b") }, Sel { db: "x".into(), ..sel("b") }].iter(), 0, &want), None);
    }
}
