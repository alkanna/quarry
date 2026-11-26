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

/// Whether the query bar of a Content or Documents tab holds something other than what the rows below came from. The rows stay
/// until Run, so the bar says so.
pub fn query_dirty(t: &Tab) -> bool {
    if t.view != crate::state::View::Documents {
        return false;
    }
    match &t.sql {
        // The SQL bar builds its query once a frame and keeps the answer, so the results do not build it again.
        Some(s) => s.bar.dirty,
        None => t.ran_txt.iter().zip(bar_refs(t)).any(|(a, b)| a != b),
    }
}

/// One text of the bar with the tab's `{{...}}` values filled in, or why it can't be, named after its field.
pub fn bar_filled(t: &Tab, text: &str, what: &str) -> crate::db::Res<String> {
    use crate::state::library::snippet_strip::{fill_tab, Src};
    fill_tab(t, text, Src::Bar, crate::library::snippet::Lang::Query, true).map_err(|e| format!("{what}: {e}"))
}

/// The Documents query from the tab's filter, sort, project, limit, skip, hint and collation boxes.
pub fn find_query(t: &Tab) -> crate::db::Res<Query> {
    use crate::db;
    use mongodb::bson::Bson;
    let doc = |s: &str, what: &str| if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some).map_err(|e| format!("{what}: {e}")) };
    // Read as i64, the server's type, so a value the server cannot take is refused here.
    let num = |s: &str, what: &str| if s.trim().is_empty() { Ok(None) } else { s.trim().parse::<i64>().ok().filter(|n| *n >= 0).map(|n| Some(n as u64)).ok_or(format!("{what}: enter a whole number")) };
    Ok(Query {
        filter: db::parse_doc(&bar_filled(t, &t.filter_txt, "Filter")?).map_err(|e| format!("Filter: {e}"))?,
        sort: doc(&bar_filled(t, &t.sort_txt, "Sort")?, "Sort")?,
        projection: doc(&bar_filled(t, &t.proj_txt, "Projection")?, "Projection")?,
        // A limit of 0 means none to the server, so it is treated as empty.
        limit: num(&t.limit_txt, "Limit")?.filter(|&l| l > 0),
        skip: num(&t.skip_txt, "Skip")?.unwrap_or(0),
        // Index keys as an object, else an index name, quoted or not.
        hint: match t.hint_txt.trim() {
            "" => None,
            h if h.starts_with('{') => Some(Bson::Document(db::parse_doc(h).map_err(|e| format!("Hint: {e}"))?)),
            h => Some(Bson::String(h.trim_matches(|c| c == '"' || c == '\'').to_string())),
        },
        collation: doc(&t.collation_txt, "Collation")?,
    })
    .and_then(|q| q.driver_collation().map(|_| q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Sel;
    use mongodb::bson::{doc, Bson};

    #[test]
    fn find_query_reads_limit_and_skip() {
        let mut t = Tab::new(Sel { conn: 0, db: "d".into(), coll: "c".into(), ..Default::default() });
        (t.filter_txt, t.limit_txt, t.skip_txt) = ("{}".into(), " 50 ".into(), "".into());
        let q = find_query(&t).unwrap();
        assert_eq!((q.limit, q.skip), (Some(50), 0));
        t.limit_txt = "0".into();
        assert_eq!(find_query(&t).unwrap().limit, None);
        t.skip_txt = "x".into();
        assert_eq!(find_query(&t).err().unwrap(), "Skip: enter a whole number");
        t.skip_txt.clear();
        (t.hint_txt, t.collation_txt) = ("'status_1'".into(), "{ locale: 'fr', strength: 2 }".into());
        let q = find_query(&t).unwrap();
        assert_eq!((q.hint, q.collation), (Some(Bson::String("status_1".into())), Some(doc! { "locale": "fr", "strength": 2 })));
        t.hint_txt = "{ status: 1 }".into();
        assert_eq!(find_query(&t).unwrap().hint, Some(Bson::Document(doc! { "status": 1 })));
        t.collation_txt = "{ strength: 'x' }".into();
        assert!(find_query(&t).err().unwrap().starts_with("Collation: "));
    }

    #[test]
    fn texts_set_without_a_run_are_edits_until_seeded() {
        let mut t = Tab::new(Sel { conn: 0, db: "d".into(), coll: "c".into(), ..Default::default() });
        assert!(!query_dirty(&t), "a new tab is out of date");
        t.filter_txt = "{ a: 1 }".into();
        assert!(query_dirty(&t), "a typed filter is not an edit");
        t.seed_ran();
        assert!(!query_dirty(&t), "a seeded tab is out of date");
        t.sort_txt = "{ a: -1 }".into();
        t.filter_txt = "{ a: 2 }".into();
        t.ran_sort();
        assert!(query_dirty(&t), "a header sort hid a filter that did not run");
    }
}
