//! The import view.

use super::*;

impl App {
    pub(crate) fn import_view(&mut self, ui: &mut egui::Ui) {
        let mut open_file = false;
        let mut start = false;
        let writable = self.sel_writable();
        let t = &mut self.tabs[self.cur];
        let im = &mut t.import;
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
                    with_autopair(ui, id, &mut im.text, |ui, text| {
                        egui::TextEdit::multiline(text).id(id).code_editor().hint_text("Paste JSON, JSON Lines or CSV here, or open a file").desired_width(f32::INFINITY).desired_rows(6).layouter(&mut code_layout).show(ui)
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
        // Parse again only when the input or options changed.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (&im.text, im.kind as u8, im.separator, im.header, im.camel).hash(&mut h);
            h.finish()
        };
        if key != im.parsed_key {
            im.parsed_key = key;
            im.parsed = (!im.text.trim().is_empty()).then(|| db::parse_import(&im.text, im.kind, &db::CsvOptions { separator: im.separator, header: im.header, camel_case: im.camel }));
            im.fields = match &im.parsed {
                Some(Ok(d)) => auto_fields(&d[..d.len().min(500)]),
                _ => Vec::new(),
            };
            t.selected = None;
        }
        ui.add_space(6.0);
        let mut preview = None;
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
                preview = Some((docs[..docs.len().min(PAGE_SIZE as usize)].to_vec(), im.fields.clone()));
            }
        }
        if open_file {
            if let Some(path) = rfd::FileDialog::new().add_filter("Data", &["json", "jsonl", "ndjson", "csv", "tsv", "txt"]).pick_file() {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let im = &mut self.tabs[self.cur].import;
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                        im.kind = match ext.as_str() {
                            "csv" | "tsv" => db::ImportKind::Csv,
                            "jsonl" | "ndjson" => db::ImportKind::JsonLines,
                            _ if text.trim_start().starts_with('{') && text.lines().filter(|l| !l.trim().is_empty()).count() > 1 && db::parse_doc(text.lines().next().unwrap_or_default()).is_ok() => db::ImportKind::JsonLines,
                            _ => db::ImportKind::Json,
                        };
                        if ext == "tsv" {
                            im.separator = Some('\t');
                        }
                        im.file = path.file_name().map(|f| f.to_string_lossy().to_string());
                        im.text = text;
                    }
                    Err(e) => self.err(format!("Could not read the file: {e}")),
                }
            }
        }
        if start {
            if !writable {
                self.ask_unlock();
            } else {
                let im = &self.tabs[self.cur].import;
                let n = match &im.parsed {
                    Some(Ok(d)) => d.len(),
                    _ => 0,
                };
                if im.upsert {
                    let key = im.upsert_by.trim().to_string();
                    let coll = self.tabs[self.cur].sel.coll.clone();
                    self.modal = Some(Modal::Typed {
                        title: format!("Upsert {n} documents into {coll} by {key}? Matching documents are updated with the imported fields."),
                        expect: coll,
                        typed: String::new(),
                        action: Danger::Upsert { key },
                    });
                } else {
                    self.modal = Some(Modal::ImportInsert(n));
                }
            }
        }
        if let Some((docs, fields)) = preview {
            // The preview uses the same table as documents.
            let t = &mut self.tabs[self.cur];
            let saved_docs = std::mem::replace(&mut t.agg_docs, docs);
            let saved_fields = std::mem::replace(&mut t.agg_fields, fields);
            self.results(ui, true);
            let t = &mut self.tabs[self.cur];
            t.agg_docs = saved_docs;
            t.agg_fields = saved_fields;
        }
    }
}
