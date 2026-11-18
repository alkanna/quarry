//! Trying a candidate index with HypoPG from the explain dialog: a button under the candidate plans the statement again
//! as if the index existed, and a line says what the plan would cost with it, or that the planner would not use it.
//! Nothing is created and nothing runs until the button is clicked. The trial itself is `db::sql::pg::hypo`.

use super::explain::{Pane, SqlExplain, XAct};
use super::explain_panel::{mark_box, Mark};
use crate::app::*;
use crate::db::sql::pg::hypo;
use crate::db::sql::plan::findings::Fix;
use crate::db::sql::plan::{self, compare, Plan};
use crate::db::sql::Dialect;

/// The answer of a trial, kept for the candidate it tried.
pub(in crate::app) struct HypoRun {
    /// The candidate's SQL, so the line shows under that candidate only.
    pub(in crate::app) fix: String,
    pub(in crate::app) plan: Plan,
    /// The plan reads through the hypothetical index.
    pub(in crate::app) used: bool,
    /// From the dialog's plan to the trial's, for Compare.
    pub(in crate::app) diff: compare::Diff,
}

/// Why the trial cannot run, shown beside the disabled button. None when it can.
fn blocked(x: &SqlExplain, fix: &Fix) -> Option<String> {
    if x.demo {
        return Some("The offline demo has no server to try the index on.".into());
    }
    if x.db.pooler() {
        return Some(hypo::POOLER.into());
    }
    if let Some(why) = extension_gate(x.ev.as_ref(), x.ev_req.is_some()) {
        return Some(why.into());
    }
    if hypo::create_sql(fix).is_none() {
        return Some("This candidate is not a CREATE INDEX HypoPG can take.".into());
    }
    hypo::refused(&x.stmt, x.locked)
}

/// Why the catalog's answer does not show HypoPG as installed. A failed read says nothing either way, unless it still saw
/// the extension.
fn extension_gate(ev: Option<&plan::Evidence>, busy: bool) -> Option<&'static str> {
    match ev {
        Some(ev) if ev.hypopg => None,
        _ if busy => Some("Checking whether HypoPG is installed."),
        Some(ev) if !ev.failed => Some("Needs the HypoPG extension in this database, usable by this login."),
        _ => Some("The catalog could not be read to check for HypoPG."),
    }
}

/// The button and the trial's answer, under the candidate `fix`. PostgreSQL only.
pub(super) fn hypo_row(ui: &mut egui::Ui, x: &SqlExplain, fix: &Fix, acts: &mut Vec<XAct>) {
    if x.dialect != Dialect::Postgres {
        return;
    }
    let busy = x.hypo_wait.as_ref().is_some_and(|(_, f)| *f == fix.sql);
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        let why = blocked(x, fix);
        let r = ui.add_enabled_ui(why.is_none() && !busy, |ui| quiet(ui, "Try it (nothing is created)")).inner;
        if r.on_hover_text("Plans the statement again with this index as a HypoPG hypothetical index, in a read-only transaction that is rolled back").clicked() {
            acts.push(XAct::Hypo(fix.clone()));
        }
        if busy {
            ui.spinner();
            ui.label(RichText::new("Planning with the hypothetical index").size(TXT_SMALL).color(MUTED()));
        } else if let Some(why) = why {
            ui.label(RichText::new(why).size(TXT_SMALL).color(FAINT()));
        }
    });
    let Some(h) = x.hypo.as_ref().filter(|h| h.fix == fix.sql && !busy) else { return };
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if h.used {
            mark_box(ui, Mark::Check, SUCCESS());
        } else {
            mark_box(ui, Mark::Info, MUTED());
        }
        ui.label(RichText::new(result_line(h, x.plan.as_ref())).size(TXT_SMALL).color(if h.used { TEXT() } else { MUTED() }));
        if quiet(ui, "Show in Compare").on_hover_text("Compare this plan with the one the server would choose with the index").clicked() {
            acts.push(XAct::HypoCompare);
        }
    });
}

/// "With this index: cost 2,704, was 5,279 (-49%)." or "The planner would not use this index."
fn result_line(h: &HypoRun, before: Option<&Plan>) -> String {
    if !h.used {
        return "The planner would not use this index.".into();
    }
    let cost = |c: f64| group(&format!("{c:.0}"));
    match (h.plan.cost, before.and_then(|p| p.cost)) {
        (Some(a), Some(b)) => format!("With this index: cost {}, was {} ({}).", cost(a), cost(b), compare::Delta { before: Some(b), after: Some(a) }.text()),
        (Some(a), None) => format!("With this index: cost {}.", cost(a)),
        _ => "The planner would use this index.".into(),
    }
}

impl App {
    /// Asks the server for the plan with the candidate `fix` as a hypothetical index. Only a click gets here.
    pub(super) fn sql_explain_hypo_try(&mut self, x: &mut SqlExplain, fix: Fix) {
        if x.dialect != Dialect::Postgres || blocked(x, &fix).is_some() {
            return;
        }
        let Some(create) = hypo::create_sql(&fix).map(str::to_string) else { return };
        self.req += 1;
        (x.hypo_wait, x.hypo, x.hypo_cmp) = (Some((self.req, fix.sql)), None, false);
        x.leave_empty_compare();
        self.worker.sql_explain_hypo(self.req, x.db.clone(), x.schema.clone(), create, x.stmt.clone(), x.locked);
    }

    /// The answer of a trial of the open dialog.
    pub(in crate::app) fn sql_explain_hypo(&mut self, req: u64, res: db::Res<hypo::Trial>) {
        let Some(Modal::SqlExplain(x)) = &mut self.modal else { return };
        let Some(fix) = x.hypo_wait.take_if(|(r, _)| *r == req).map(|(_, f)| f) else { return };
        let t = match res {
            Ok(t) => t,
            Err(e) => return self.err(e),
        };
        let mut p = match plan::parse(Dialect::Postgres, &t.plan) {
            Ok(p) => p,
            Err(e) => return self.err(e),
        };
        // The same evidence as the dialog's plan, so Compare sets like findings against like.
        p.refind(x.dialect, x.ev.as_ref(), x.db.pooler());
        let Some(before) = &x.plan else { return };
        let diff = compare::diff(before, &p);
        x.hypo = Some(HypoRun { fix, used: hypo::uses(&p, &t.index), plan: p, diff });
    }

    /// Opens the trial's plan in Compare, against the dialog's own.
    pub(super) fn sql_explain_hypo_compare(&mut self, x: &mut SqlExplain) {
        if x.hypo.is_some() {
            (x.hypo_cmp, x.pane) = (true, Pane::Compare);
        }
    }

    /// A trial's answer as if the server had sent it, for the candidate of the open dialog.
    #[cfg(test)]
    pub(in crate::app) fn demo_explain_hypo(&mut self, trial: hypo::Trial) {
        let Some(Modal::SqlExplain(x)) = &mut self.modal else { return };
        let fix = x.plan.as_ref().and_then(|p| p.findings.iter().find_map(|f| f.fix.as_ref().filter(|fx| fx.kind == plan::FixKind::Index))).map(|fx| fx.sql.clone()).expect("a candidate index");
        self.req += 1;
        x.hypo_wait = Some((self.req, fix));
        let req = self.req;
        self.sql_explain_hypo(req, Ok(trial));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_catalog_read_is_not_taken_for_a_missing_extension() {
        let ev = |failed, hypopg| plan::Evidence { failed, hypopg, ..Default::default() };
        assert_eq!(extension_gate(Some(&ev(true, false)), false), Some("The catalog could not be read to check for HypoPG."));
        assert_eq!(extension_gate(Some(&ev(true, false)), true), Some("Checking whether HypoPG is installed."));
        assert_eq!(extension_gate(Some(&ev(false, false)), false), Some("Needs the HypoPG extension in this database, usable by this login."));
        // The tables' reads failed, but the extensions were seen.
        assert_eq!(extension_gate(Some(&ev(true, true)), false), None);
        assert_eq!(extension_gate(None, true), Some("Checking whether HypoPG is installed."));
        assert_eq!(extension_gate(None, false), Some("The catalog could not be read to check for HypoPG."));
    }
}
