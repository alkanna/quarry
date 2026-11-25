//! Runs the query bar, pasted shell queries and pipelines.

use super::*;
use quarry_core::state::run::bar_refs;

impl App {
    pub(super) fn run_query(&mut self) {
        let Some(t) = self.t() else { return };
        if db::is_shell(&t.filter_txt) {
            return match bar_filled(t, &t.filter_txt, "Filter") {
                Ok(text) => self.run_shell(&text),
                Err(e) => self.tm().unwrap().bar_err = Some(e),
            };
        }
        let q = find_query(t);
        match q {
            Ok(q) => {
                let (f, so, p) = (t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone());
                let key = self.col_key(false);
                let h = self.prefs.history.entry(key).or_default();
                push_history(&mut h.selector, &f);
                push_history(&mut h.sort, &so);
                push_history(&mut h.project, &p);
                self.save_prefs();
                self.tm().unwrap().apply_find(q);
                self.reload();
            }
            // Nothing ran, so the message goes under the bar and the rows stay.
            Err(e) => self.tm().unwrap().bar_err = Some(e),
        }
    }

    /// Opens the current query or pipeline as a snippet for one language.
    pub(super) fn show_code(&mut self, lang: db::Lang, find: bool) {
        let Some(t) = self.t() else { return };
        let from_shell = |s: &str| db::parse_shell(s).map(|sh| match sh.kind {
            db::ShellKind::Find(q) => db::CodeOf::Find(Box::new(q)),
            db::ShellKind::Aggregate(p) => db::CodeOf::Aggregate(p),
        });
        let what = if find && !db::is_shell(&t.filter_txt) {
            find_query(t).map(|q| db::CodeOf::Find(Box::new(q)))
        } else if find {
            // What runs, or the text as written when a value does not read yet.
            from_shell(&bar_filled(t, &t.filter_txt, "Filter").unwrap_or_else(|_| t.filter_txt.clone()))
        } else if db::is_shell(&t.agg_txt) {
            from_shell(&t.agg_txt)
        } else {
            db::parse_pipeline(&t.agg_txt).map(db::CodeOf::Aggregate)
        };
        match what {
            Ok(w) => self.modal = Some(Modal::Text { title: format!("{} code", lang.name()), body: db::driver_code(lang, &t.sel.db, &t.sel.coll, &w), code: false }),
            Err(e) => self.err(e),
        }
    }

    /// Runs a pasted `db.<coll>.find(...)` or `db.<coll>.aggregate([...])`, switching collection if needed.
    pub(super) fn run_shell(&mut self, text: &str) {
        let sh = match db::parse_shell(text) {
            Ok(s) => s,
            Err(e) => {
                match self.tm().filter(|t| t.view == View::Documents) {
                    Some(t) => t.bar_err = Some(e),
                    None => self.err(e),
                }
                return;
            }
        };
        let Some(cur) = self.t().map(|t| t.sel.clone()) else { return self.warn("Open a collection first.") };
        if let Some(t) = self.tm() {
            t.bar_err = None;
        }
        if sh.coll != cur.coll {
            // The pasted text belongs to the other collection, so this tab's bar goes back to what it last ran.
            if let Some(t) = self.tm() {
                t.filter_txt = t.ran_txt[0].clone();
            }
            // Open::Link never takes a tab that holds work, so another collection's own query is not overwritten.
            self.open_collection(Sel { coll: sh.coll.clone(), ..cur }, Open::Link);
        }
        let t = self.tm().unwrap();
        t.selected = None;
        match sh.kind {
            ShellKind::Find(q) => {
                t.view = View::Documents;
                t.filter_txt = text.trim().to_string();
                t.clear_options();
                t.apply_find(q);
                self.reload();
            }
            ShellKind::Aggregate(p) => {
                t.view = View::Aggregate;
                t.agg_txt = text.trim().to_string();
                self.run_pipeline(p);
            }
        }
    }

    pub(super) fn run_pipeline(&mut self, p: Vec<Document>) {
        self.tm().unwrap().agg_stage = None;
        if db::has_write_stage(&p) {
            if self.writable_target().is_some() {
                self.modal = Some(Modal::AggWrite(p));
            }
        } else if self.demo {
            self.ok("Offline demo: nothing to run.");
        } else {
            match self.target() {
                Some(target) => {
                    self.req += 1;
                    let req = self.req;
                    self.tm().unwrap().agg_req = req;
                    self.worker.aggregate(req, target, p);
                }
                None => self.warn_offline(),
            }
        }
    }
}

/// Whether the query bar of a Content or Documents tab holds something other than what the rows below came from. The rows stay
/// until Run, so the bar says so.
pub(super) fn query_dirty(t: &Tab) -> bool {
    if t.view != View::Documents {
        return false;
    }
    match &t.sql {
        // The SQL bar builds its query once a frame and keeps the answer, so the results do not build it again.
        Some(s) => s.bar.dirty,
        None => t.ran_txt.iter().zip(bar_refs(t)).any(|(a, b)| a != b),
    }
}

/// The Documents query from the tab's filter, sort, project, limit, skip, hint and collation boxes.
/// One text of the bar with the tab's `{{...}}` values filled in, or why it can't be, named after its field.
fn bar_filled(t: &Tab, text: &str, what: &str) -> db::Res<String> {
    use crate::app::library::snippet_strip::{fill_tab, Src};
    fill_tab(t, text, Src::Bar, crate::library::snippet::Lang::Query, true).map_err(|e| format!("{what}: {e}"))
}

pub(super) fn find_query(t: &Tab) -> db::Res<Query> {
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
