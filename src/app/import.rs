//! The import view.

use super::*;
use quarry_core::action::{ImportA, MongoA};
use quarry_core::core::TextRef;

impl App {
    pub(super) fn import_view(&mut self, ui: &mut egui::Ui) {
        let mut open_file = false;
        let mut start = false;
        let writable = self.sel_writable();
        let t = &self.core.tabs[self.core.cur];
        let tab = t.uid;
        let im = &t.import;
        // Copied out so the widgets edit copies and the source box can take the text itself.
        let before = (im.kind, im.separator, im.header, im.camel, im.upsert);
        let (mut kind, mut separator, mut header, mut camel, mut upsert) = before;
        let mut upsert_by = im.upsert_by.clone();
        let (file, text_len) = (im.file.clone(), im.text.len());
        let parsed = match &im.parsed {
            None => None,
            Some(Err(e)) => Some(Err(e.clone())),
            Some(Ok(d)) => Some(Ok((d.len(), im.fields.len()))),
        };
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, "IMPORT");
                segmented(ui, &mut kind, &[(db::ImportKind::Json, "JSON"), (db::ImportKind::JsonLines, "JSON Lines"), (db::ImportKind::Csv, "CSV")]);
                if kind == db::ImportKind::Csv {
                    ui.add_space(8.0);
                    ui.checkbox(&mut header, "First row is the header");
                    ui.checkbox(&mut camel, "camelCase names");
                    egui::ComboBox::from_id_salt("csv-sep")
                        .selected_text(match separator {
                            None => "separator: auto",
                            Some('\t') => "separator: tab",
                            Some(',') => "separator: ,",
                            Some(';') => "separator: ;",
                            _ => "separator: |",
                        })
                        .show_ui(ui, |ui| {
                            menu_look(ui);
                            for (v, l) in [(None, "auto"), (Some(','), ","), (Some(';'), ";"), (Some('\t'), "tab"), (Some('|'), "|")] {
                                ui.selectable_value(&mut separator, v, l);
                            }
                        });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_file = ui.button("Open file…").clicked();
                    if let Some(f) = &file {
                        ui.label(RichText::new(f).size(TXT_SMALL).color(MUTED()));
                    }
                });
            });
            let id = ui.make_persistent_id("import-text");
            // Very large files are not shown in full; the preview below shows what will be imported.
            const IMPORT_SHOW_MAX: usize = 200_000;
            if text_len <= IMPORT_SHOW_MAX {
                egui::ScrollArea::vertical().id_salt("import-src").max_height(180.0).show(ui, |ui| {
                    if let Some(text) = self.core.text_mut(TextRef::ImportText) {
                        crate::ui::vim::wrap(ui, id, text, |ui, text| {
                            with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().hint_text("Paste JSON, JSON Lines or CSV here, or open a file").desired_width(f32::INFINITY).desired_rows(6).layouter(&mut code_layout).show(ui))
                        });
                    }
                });
            } else {
                ui.label(RichText::new(format!("{} loaded from the file (too large to show).", human_bytes(text_len))).color(MUTED()));
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut upsert, false, "Insert as new documents");
                ui.radio_value(&mut upsert, true, "Upsert by");
                ui.add_enabled(upsert, egui::TextEdit::singleline(&mut upsert_by).desired_width(110.0).font(egui::TextStyle::Monospace));
                ui.label(RichText::new("(update the match, add if none)").size(TXT_SMALL).color(FAINT()));
            });
        });
        ui.add_space(6.0);
        let mut preview = false;
        match &parsed {
            None => {
                ui.label(RichText::new("Nothing to import yet.").color(MUTED()));
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).monospace().color(ALERT()));
            }
            Some(Ok((n_docs, n_fields))) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} documents ready", group(&n_docs.to_string()))).color(BRIGHT()));
                    ui.label(RichText::new(format!("·  {} fields  ·  preview of the first {}", n_fields, (*n_docs).min(PAGE_SIZE as usize))).color(MUTED()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = format!("{} {} documents", if upsert { "Upsert" } else { "Import" }, group(&n_docs.to_string()));
                        start = ui.add_enabled(*n_docs > 0, primary_button(ui, label, if writable { PRIMARY() } else { FAINT() })).clicked();
                        if !writable {
                            ui.label(RichText::new("needs writes unlocked").size(TXT_SMALL).color(FAINT()));
                        }
                    });
                });
                preview = true;
            }
        }
        let mut acts = Vec::new();
        if (kind, separator, header, camel, upsert) != before {
            acts.push(ImportA::Opts { tab, kind, separator, header, camel, upsert });
        }
        if upsert_by != self.core.tabs[self.core.cur].import.upsert_by {
            acts.push(ImportA::UpsertBy { tab, field: upsert_by });
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
