//! Runs the query bar, pasted shell queries and pipelines.

use super::*;

impl App {
    pub(super) fn run_query(&mut self) {
        let Some(t) = self.t() else { return };
        if db::is_shell(&t.filter_txt) {
            let text = t.filter_txt.clone();
            return self.run_shell(&text);
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
                let t = self.tm().unwrap();
                t.query = q;
                t.page = 0;
                t.bar_err = None;
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
            from_shell(&t.filter_txt)
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
            self.open_collection(Sel { coll: sh.coll.clone(), ..cur }, Open::Replace);
        }
        let t = self.tm().unwrap();
        t.selected = None;
        match sh.kind {
            ShellKind::Find(q) => {
                t.view = View::Documents;
                t.filter_txt = text.trim().to_string();
                t.clear_options();
                t.query = q;
                t.page = 0;
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

/// The Documents query from the tab's filter, sort, project, limit, skip, hint and collation boxes.
pub(super) fn find_query(t: &Tab) -> db::Res<Query> {
    let doc = |s: &str, what: &str| if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some).map_err(|e| format!("{what}: {e}")) };
    // Read as i64, the server's type, so a value the server cannot take is refused here.
    let num = |s: &str, what: &str| if s.trim().is_empty() { Ok(None) } else { s.trim().parse::<i64>().ok().filter(|n| *n >= 0).map(|n| Some(n as u64)).ok_or(format!("{what}: enter a whole number")) };
    Ok(Query {
        filter: db::parse_doc(&t.filter_txt).map_err(|e| format!("Filter: {e}"))?,
        sort: doc(&t.sort_txt, "Sort")?,
        projection: doc(&t.proj_txt, "Projection")?,
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
}
