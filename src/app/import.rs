//! The import view.

use super::*;
use quarry_core::action::{ImportA, MongoA};

impl App {
    pub(super) fn import_view(&mut self, ui: &mut egui::Ui) {
        let mut open_file = false;
        let mut start = false;
        let writable = self.sel_writable();
        // Typing in the box changes the input, so it is parsed again here rather than by an action.
        self.core.import_parse();
        let t = &mut self.core.tabs[self.core.cur];
        let tab = t.uid;
        let im = &mut t.import;
        let opts = |im: &ImportTab| (im.kind, im.separator, im.header, im.camel, im.upsert);
        let before = opts(im);
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, "IMPORT");
                segmented(ui, &mut im.kind, &[(db::ImportKind::Json, "JSON"), (db::ImportKind::JsonLines, "JSON Lines"), (db::ImportKind::Csv, "CSV")]);
                if im.kind == db::ImportKind::Csv {
                    ui.add_space(8.0);
                    ui.checkbox(&mut im.header, "First row is the header");
                    ui.checkbox(&mut im.camel, "camelCase names");
                    egui::ComboBox::from_id_salt("csv-sep")
                        .selected_text(match im.separator {
                            None => "separator: auto",
                            Some('\t') => "separator: tab",
                            Some(',') => "separator: ,",
                            Some(';') => "separator: ;",
                            _ => "separator: |",
                        })
                        .show_ui(ui, |ui| {
                            menu_look(ui);
                            for (v, l) in [(None, "auto"), (Some(','), ","), (Some(';'), ";"), (Some('\t'), "tab"), (Some('|'), "|")] {
                                ui.selectable_value(&mut im.separator, v, l);
                            }
                        });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_file = ui.button("Open file…").clicked();
                    if let Some(f) = &im.file {
                        ui.label(RichText::new(f).size(TXT_SMALL).color(MUTED()));
                    }
                });
            });
            let id = ui.make_persistent_id("import-text");
            // Very large files are not shown in full; the preview below shows what will be imported.
            const IMPORT_SHOW_MAX: usize = 200_000;
            if im.text.len() <= IMPORT_SHOW_MAX {
                egui::ScrollArea::vertical().id_salt("import-src").max_height(180.0).show(ui, |ui| {
                    crate::ui::vim::wrap(ui, id, &mut im.text, |ui, text| {
                        with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().hint_text("Paste JSON, JSON Lines or CSV here, or open a file").desired_width(f32::INFINITY).desired_rows(6).layouter(&mut code_layout).show(ui))
                    });
                });
            } else {
                ui.label(RichText::new(format!("{} loaded from the file (too large to show).", human_bytes(im.text.len()))).color(MUTED()));
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut im.upsert, false, "Insert as new documents");
                ui.radio_value(&mut im.upsert, true, "Upsert by");
                ui.add_enabled(im.upsert, egui::TextEdit::singleline(&mut im.upsert_by).desired_width(110.0).font(egui::TextStyle::Monospace));
                ui.label(RichText::new("(update the match, add if none)").size(TXT_SMALL).color(FAINT()));
            });
        });
        ui.add_space(6.0);
        let mut preview = false;
        match &im.parsed {
            None => {
                ui.label(RichText::new("Nothing to import yet.").color(MUTED()));
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).monospace().color(ALERT()));
            }
            Some(Ok(docs)) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} documents ready", group(&docs.len().to_string()))).color(BRIGHT()));
                    ui.label(RichText::new(format!("·  {} fields  ·  preview of the first {}", im.fields.len(), docs.len().min(PAGE_SIZE as usize))).color(MUTED()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = format!("{} {} documents", if im.upsert { "Upsert" } else { "Import" }, group(&docs.len().to_string()));
                        start = ui.add_enabled(!docs.is_empty(), primary_button(ui, label, if writable { PRIMARY() } else { FAINT() })).clicked();
                        if !writable {
                            ui.label(RichText::new("needs writes unlocked").size(TXT_SMALL).color(FAINT()));
                        }
                    });
                });
                preview = true;
            }
        }
        let after = opts(im);
        let mut acts = Vec::new();
        if after != before {
            // The widgets wrote into the tab; the action is what applies the change.
            (im.kind, im.separator, im.header, im.camel, im.upsert) = before;
            let (kind, separator, header, camel, upsert) = after;
            acts.push(ImportA::Opts { tab, kind, separator, header, camel, upsert });
        }
        if open_file && let Some(path) = rfd::FileDialog::new().add_filter("Data", &["json", "jsonl", "ndjson", "csv", "tsv", "txt"]).pick_file() {
            acts.push(ImportA::File { tab, path });
        }
        if start {
            acts.push(ImportA::Start { tab });
        }
        for a in acts {
            self.exec(Action::Mongo(MongoA::Import(a)));
        }
        if preview {
            // The preview uses the same table as documents.
            self.results(ui, true);
        }
    }
}
