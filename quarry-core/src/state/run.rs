//! State behind src/app/run.rs, which draws it.

use crate::db::Query;
use crate::state::Tab;

impl Tab {
    /// Shows the rows of a find query read from the bar: the page starts over and the bar's texts are what ran.
    pub fn apply_find(&mut self, q: Query) {
        self.query = q;
        self.page = 0;
        self.bar_err = None;
        self.ran_txt = bar_refs(self).map(|s| s.clone());
    }

    /// A header sort runs the new sort with the rest of the query that ran before, so only the sort is recorded as run: the
    /// other boxes may hold edits that did not run.
    pub fn ran_sort(&mut self) {
        self.ran_txt[1] = self.sort_txt.clone();
    }

    /// Takes what the query line holds as what ran, for a tab whose texts were set without a run: restored, duplicated or
    /// built by the demo. A run or a page load records what ran itself.
    pub fn seed_ran(&mut self) {
        self.ran_txt = bar_refs(self).map(|s| s.clone());
        if self.sql.is_some() {
            let q = super::sql::bar::page_query(self);
            if let Some(s) = self.sql.as_mut() {
                s.ran = q;
            }
        }
    }
}

pub fn bar_refs(t: &Tab) -> [&String; 7] {
    [&t.filter_txt, &t.sort_txt, &t.proj_txt, &t.limit_txt, &t.skip_txt, &t.hint_txt, &t.collation_txt]
}
