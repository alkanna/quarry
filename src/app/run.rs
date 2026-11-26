//! Runs the query bar, pasted shell queries and pipelines. The runs are in Core (`quarry_core::action::docs`); the
//! code snippets of a query open here.

use super::*;
use quarry_core::action::{Action, DocsA};
pub(super) use quarry_core::state::run::{bar_filled, find_query, query_dirty};

impl App {
    pub(super) fn run_query(&mut self) {
        if let Some(tab) = self.cur_uid() {
            self.exec(Action::Docs(DocsA::Run { tab }));
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

    pub(super) fn run_shell(&mut self, text: &str) {
        if let Some(tab) = self.cur_uid() {
            self.exec(Action::Docs(DocsA::RunShell { tab, text: text.to_string() }));
        }
    }

    pub(super) fn run_pipeline(&mut self, pipeline: Vec<Document>) {
        if let Some(tab) = self.cur_uid() {
            self.exec(Action::Docs(DocsA::RunPipeline { tab, pipeline }));
        }
    }
}
