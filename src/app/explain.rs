//! The explain dialog: a readable summary of how the server ran a query, with the raw output.

use super::*;

impl App {
    pub(super) fn explain_dialog(&mut self, ui: &mut egui::Ui, summary: &mut db::ExplainSummary, raw: &str, show_raw: &mut bool, keep: &mut bool) {
        ui.set_width(720.0);
        dialog_title(ui, "Explain");
        ui.label(RichText::new(format!("{} returned  ·  {} keys examined  ·  {} documents examined  ·  {} ms", group(&summary.returned.to_string()), group(&summary.keys.to_string()), group(&summary.docs.to_string()), summary.ms)).color(MUTED()));
        for w in &summary.warnings {
            ui.label(RichText::new(w.as_str()).color(WARNING()));
        }
        ui.horizontal(|ui| {
            segmented(ui, show_raw, &[(false, "Stages"), (true, "Raw JSON")]);
        });
        let find = if *show_raw { Find::bar(ui, egui::Id::unique("explain-raw"), raw) } else { Find::default() };
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            if *show_raw {
                let mut text = raw;
                let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(true)).show(ui);
                find.reveal(ui, raw, &out);
            } else if summary.rows.is_empty() {
                ui.label(RichText::new("The server returned no execution stages. See Raw JSON.").color(MUTED()));
            } else {
                egui::Grid::new("explain-stages").num_columns(6).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                    for h in ["STAGE", "INDEX", "KEYS", "DOCS", "RETURNED", "MS"] {
                        caption(ui, h);
                    }
                    ui.end_row();
                    let n = |v: Option<i64>| v.map(|v| group(&v.to_string())).unwrap_or_default();
                    for r in &summary.rows {
                        let c = match r.stage.to_lowercase().as_str() {
                            "ixscan" | "ixseek" => SUCCESS(),
                            "collscan" | "scan" => ALERT(),
                            _ => TEXT(),
                        };
                        ui.label(RichText::new(format!("{}{}", "  ".repeat(r.depth), r.stage)).monospace().color(c));
                        ui.label(RichText::new(r.index.as_str()).monospace().color(MUTED()));
                        for v in [r.keys, r.docs, r.returned, r.ms] {
                            ui.label(RichText::new(n(v)).monospace());
                        }
                        ui.end_row();
                    }
                });
            }
        });
        dialog_actions(ui, |ui| {
            if ui.button("Copy raw").clicked() {
                ui.ctx().copy_text(raw.to_string());
            }
            if ui.button("Close").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }
}

impl App {
    /// Explains the Documents query or the Aggregate pipeline, whichever view is open.
    pub(super) fn explain(&mut self) {
        let Some(t) = self.t() else { return };
        let what = match t.view {
            View::Documents if !db::is_shell(&t.filter_txt) => Ok(db::ExplainOf::Find(Box::new(t.query.clone()))),
            View::Aggregate if !db::is_shell(&t.agg_txt) => db::parse_pipeline(&t.agg_txt).and_then(|p| if db::has_write_stage(&p) { Err("Explain would run the write stage.".into()) } else { Ok(db::ExplainOf::Aggregate(p)) }),
            View::Documents | View::Aggregate => Err("Run the pasted query first, then explain it.".into()),
            _ => return,
        };
        match (what, self.target()) {
            _ if self.demo => self.ok("Offline demo: nothing to explain."),
            (Err(e), _) => self.err(e),
            (Ok(w), Some(target)) => {
                self.req += 1;
                self.explain_req = self.req;
                self.worker.explain(self.req, target, w, self.page_size());
            }
            _ => self.warn_offline(),
        }
    }
}
