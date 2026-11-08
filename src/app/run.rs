//! Runs the query bar, pasted shell queries and pipelines.

use super::*;

impl App {
    pub(super) fn run_query(&mut self) {
        let Some(t) = self.t() else { return };
        if db::is_shell(&t.filter_txt) {
            let text = t.filter_txt.clone();
            return self.run_shell(&text);
        }
        let parse = |s: &str| -> db::Res<Option<Document>> { if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some) } };
        let q = (|| -> db::Res<Query> {
            Ok(Query {
                filter: db::parse_doc(&t.filter_txt).map_err(|e| format!("Filter: {e}"))?,
                sort: parse(&t.sort_txt).map_err(|e| format!("Sort: {e}"))?,
                projection: parse(&t.proj_txt).map_err(|e| format!("Projection: {e}"))?,
                ..Default::default()
            })
        })();
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
                self.reload();
            }
            Err(e) => self.err(e),
        }
    }

    /// Opens the current query or pipeline as a snippet for one language.
    pub(super) fn show_code(&mut self, lang: db::Lang, find: bool) {
        let Some(t) = self.t() else { return };
        let from_shell = |s: &str| db::parse_shell(s).map(|sh| match sh.kind {
            db::ShellKind::Find(q) => db::CodeOf::Find(q),
            db::ShellKind::Aggregate(p) => db::CodeOf::Aggregate(p),
        });
        let opt = |s: &str, what: &str| if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some).map_err(|e| format!("{what}: {e}")) };
        let what = if find && !db::is_shell(&t.filter_txt) {
            (|| Ok(db::CodeOf::Find(Query { filter: db::parse_doc(&t.filter_txt).map_err(|e| format!("Filter: {e}"))?, sort: opt(&t.sort_txt, "Sort")?, projection: opt(&t.proj_txt, "Projection")?, ..Default::default() })))()
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
            Err(e) => return self.err(e),
        };
        let Some(cur) = self.t().map(|t| t.sel.clone()) else { return self.err("Open a collection first.") };
        if sh.coll != cur.coll {
            self.open_collection(Sel { coll: sh.coll.clone(), ..cur }, true);
        }
        let t = self.tm().unwrap();
        t.selected = None;
        match sh.kind {
            ShellKind::Find(q) => {
                t.view = View::Documents;
                t.filter_txt = text.trim().to_string();
                t.sort_txt.clear();
                t.proj_txt.clear();
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
                None => self.err("Not connected."),
            }
        }
    }
}
