//! The Aggregate view: the pipeline builder's stages and their previews, running the pipeline or the output after one
//! stage, and the answers. src/app/aggregate.rs draws it.

use super::{Refusal, Reply};
use crate::core::Core;
use crate::db;
use crate::state::*;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AggA {
    /// What the stage cards asked for this frame, in order.
    Stages { tab: u64, acts: Vec<StageA> },
    /// Stages or Text.
    TextMode { tab: u64, on: bool },
    AutoPreview(bool),
    /// Run: the whole pipeline, or a pasted `db.coll.aggregate(...)`.
    Run { tab: u64 },
    /// A stage chip of Text mode: the output after stage `stage`.
    RunTo { tab: u64, stage: usize },
}

/// A stage card's click or edit.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum StageA {
    Edited(usize),
    Toggled(usize),
    /// Move the stage at the first index to the second, dragged there.
    Move(usize, usize),
    Remove(usize),
    Add(String),
    Op(usize, String),
    /// Show 10 more documents; `usize::MAX` is the collection sample.
    More(usize),
    Preview(usize),
}

impl Core {
    pub(super) fn aggregate(&mut self, a: AggA) -> Result<Reply, Refusal> {
        match a {
            AggA::Stages { tab, acts } => {
                self.cur_tab(tab)?;
                self.apply_stage_acts(acts);
            }
            AggA::TextMode { tab, on } => {
                self.cur_tab(tab)?;
                self.tabs[self.cur].agg_text_mode = on;
            }
            AggA::AutoPreview(on) => {
                if on != self.prefs.auto_preview {
                    self.prefs.auto_preview = on;
                    self.save_prefs();
                    if on && self.t().is_some() {
                        self.stages_changed(0, false);
                    }
                }
            }
            AggA::Run { tab } => {
                self.cur_tab(tab)?;
                let text = self.tabs[self.cur].agg_txt.clone();
                if db::is_shell(&text) {
                    self.run_shell(&text);
                } else {
                    match db::parse_pipeline(&text) {
                        Err(e) => self.err(e),
                        Ok(p) => self.run_pipeline(p),
                    }
                }
            }
            AggA::RunTo { tab, stage } => {
                self.cur_tab(tab)?;
                if let Some(p) = db::parse_pipeline(&self.tabs[self.cur].agg_txt).ok().and_then(|p| db::stage_preview(&p, stage)) {
                    self.run_pipeline(p);
                    self.tm().unwrap().agg_stage = Some(stage);
                }
            }
        }
        Ok(Reply::Done)
    }

    /// Rebuilds the builder's stages when the pipeline text changed elsewhere (Text mode, the
    /// library, a pasted query).
    pub fn sync_stages(&mut self) {
        let Some(t) = self.tm() else { return };
        if t.agg_text_mode || t.stages_src == t.agg_txt || db::is_shell(&t.agg_txt) {
            return;
        }
        if let Ok(st) = stages_from_text(&t.agg_txt) {
            t.stages = st;
            t.stages_src = t.agg_txt.clone();
        }
    }

    /// Stages from `from` on need a new preview (a second later while typing), and the pipeline
    /// text follows the stages.
    pub fn stages_changed(&mut self, from: usize, typing: bool) {
        let auto = self.prefs.auto_preview;
        let t = self.tm().unwrap();
        let at = Instant::now() + std::time::Duration::from_millis(if typing { 1000 } else { 0 });
        for s in t.stages.iter_mut().skip(from) {
            s.due = auto.then_some(at);
        }
        t.agg_txt = text_from_stages(&t.stages);
        t.stages_src = t.agg_txt.clone();
    }

    pub fn apply_stage_acts(&mut self, acts: Vec<StageA>) {
        for a in acts {
            let t = self.tm().unwrap();
            match a {
                StageA::Edited(i) => self.stages_changed(i, true),
                StageA::Toggled(i) => self.stages_changed(i, false),
                StageA::Move(i, j) => {
                    if i < t.stages.len() && j < t.stages.len() && i != j {
                        let s = t.stages.remove(i);
                        t.stages.insert(j, s);
                        self.stages_changed(i.min(j), false);
                    }
                }
                StageA::Remove(i) => {
                    if i < t.stages.len() {
                        t.stages.remove(i);
                        self.stages_changed(i, false);
                    }
                }
                StageA::Add(op) => {
                    let body = db::STAGES.iter().find(|(o, _)| *o == op).map_or("{}", |(_, b)| b);
                    t.stages.push(Stage::new(&op, body, true));
                    let i = t.stages.len() - 1;
                    self.stages_changed(i, false);
                }
                StageA::Op(i, op) => {
                    let s = &mut t.stages[i];
                    // A body still equal to the old stage's example becomes the new stage's example.
                    let template = |o: &str| db::STAGES.iter().find(|(x, _)| *x == o).map(|(_, b)| *b);
                    if template(&s.op) == Some(s.text.as_str()) || s.text.trim().is_empty() {
                        s.text = template(&op).unwrap_or("{}").to_string();
                    }
                    s.op = op;
                    self.stages_changed(i, false);
                }
                StageA::More(i) => {
                    let s = if i == usize::MAX { &mut t.agg_input } else { &mut t.stages[i] };
                    s.limit += db::STAGE_SAMPLE;
                    s.due = Some(Instant::now());
                }
                StageA::Preview(i) => t.stages[i].due = Some(Instant::now()),
            }
        }
    }

    /// Runs the next stage preview that is due, one at a time and in order, like the original's
    /// getResultsForStages (renderer line 112079), so a pipeline never floods the server. Returns when the next one is
    /// due, for the UI to wake then.
    pub fn pump_previews(&mut self) -> Option<Instant> {
        if self.demo {
            return None;
        }
        let target = self.target()?;
        let now = Instant::now();
        let t = &mut self.tabs[self.cur];
        if t.agg_input.req == 0 && t.agg_input.due.is_some_and(|d| d <= now) {
            t.agg_input.due = None;
            self.req += 1;
            t.agg_input.req = self.req;
            self.worker.stage_preview(self.req, target.clone(), Vec::new(), t.agg_input.limit);
        }
        if t.stages.iter().any(|s| s.req != 0) {
            return None;
        }
        let wake = t.stages.iter().filter_map(|s| s.due).min().filter(|next| *next > now);
        let Some(k) = t.stages.iter().position(|s| s.due.is_some_and(|d| d <= now)) else { return wake };
        t.stages[k].due = None;
        if !t.stages[k].on {
            return wake;
        }
        match stage_pipeline(&t.stages, k) {
            Err(e) => (t.stages[k].error, t.stages[k].count, t.stages[k].sample) = (Some(e), None, Vec::new()),
            // The original never previews past $out or $merge (lastStages, renderer line 112045).
            Ok(p) if db::has_write_stage(&p) => {
                (t.stages[k].error, t.stages[k].count, t.stages[k].sample) = (Some("This stage writes, so it only runs with the whole pipeline (Run).".into()), None, Vec::new());
            }
            Ok(p) => {
                self.req += 1;
                t.stages[k].req = self.req;
                self.worker.stage_preview(self.req, target, p, t.stages[k].limit);
            }
        }
        wake
    }

    /// The answer to a stage preview `req`, for whichever tab's stage asked.
    pub(super) fn stage_previewed(&mut self, req: u64, res: db::Res<(Option<u64>, Vec<mongodb::bson::Document>, u128)>) {
        let Some(st) = self.tabs.iter_mut().flat_map(|t| std::iter::once(&mut t.agg_input).chain(t.stages.iter_mut())).find(|s| s.req == req) else { return };
        st.req = 0;
        match res {
            Ok((n, docs, ms)) => {
                // Rows stay open when a refresh brings back the same documents.
                if st.sample != docs {
                    st.open.clear();
                }
                (st.count, st.sample, st.ms, st.error) = (n, docs, Some(ms), None);
            }
            Err(e) => (st.count, st.sample, st.ms, st.error) = (None, Vec::new(), None, Some(e)),
        }
    }

    /// The answer to a pipeline run `req`.
    pub(super) fn aggregated(&mut self, req: u64, res: db::Res<(Vec<db::Row>, u128)>) {
        let Some(i) = self.tabs.iter().position(|t| t.agg_req == req) else {
            // A superseded run or a closed tab: its error goes to the list, not over the newer run.
            if let Err(e) = res {
                self.err_quiet(e, None, None, "Aggregate".into());
            }
            return;
        };
        let t = &mut self.tabs[i];
        t.agg_req = 0;
        t.rows_replaced();
        match res {
            Ok((d, ms)) => {
                t.clear_failed(View::Aggregate);
                t.agg_fields = auto_fields(&d);
                let n = d.len();
                t.agg_docs = d;
                t.expanded.retain(|e| !e.0);
                t.agg_ms = Some(ms);
                self.ok(format!("Pipeline returned {n} documents"));
            }
            Err(e) => {
                // Old results under a failed run would read as its output.
                (t.agg_docs, t.agg_ms) = (Vec::new(), None);
                self.fail_view(i, View::Aggregate, e, "run the pipeline");
            }
        }
    }
}
