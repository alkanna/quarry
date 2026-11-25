//! The Content grid of a SQL tab: its page loads, the bar's filter, sort and paging, and the answers that fill them.
//! Drawn by src/app/sql/views.rs, bar.rs and edit.rs.

use crate::core::Core;
use crate::db;
use crate::state::config::push_history;

impl Core {
    /// Asks for the tab's current page. The answer is matched back to the tab by `req`.
    pub fn reload_sql(&mut self, i: usize) {
        // A Query tab that belongs to no table has no page to load.
        if self.tabs.get(i).is_some_and(|t| t.sel.coll.is_empty()) {
            return;
        }
        let Some((db, space)) = self.tabs.get(i).and_then(|t| self.sql_target(&t.sel)) else { return };
        self.req += 1;
        let (req, size) = (self.req, self.page_size());
        let t = &mut self.tabs[i];
        (t.req, t.loading, t.size) = (req, true, size);
        let q = crate::state::sql::bar::page_query(t);
        if let Some(s) = t.sql.as_mut() {
            s.ran = q.clone();
            s.bar.dirty = false;
        }
        self.worker.sql_page(req, db, space, t.sel.coll.clone(), q, size, t.page * size);
    }

    /// Puts the WHERE and ORDER BY of tab `i`'s page in the table's history once the page loaded, so a filter the server
    /// refused is not kept. They are saved in ui.json and listed in the menu, so a password in them is masked.
    pub fn sql_remember_filter(&mut self, i: usize) {
        let Some(t) = self.tabs.get(i).filter(|t| !t.sel.coll.is_empty()) else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let (key, filter, order) = (t.coll_key(), db::redact_sql(&s.ran.filter).0, db::redact_sql(&s.ran.order).0);
        let h = self.prefs.history.entry(key).or_default();
        let before = (h.selector.first().cloned(), h.sort.first().cloned());
        push_history(&mut h.selector, &filter);
        push_history(&mut h.sort, &order);
        // Paging and reloads repeat the same texts, which leave the lists as they were and need no write.
        if (h.selector.first().cloned(), h.sort.first().cloned()) != before {
            self.save_prefs();
        }
    }
}
