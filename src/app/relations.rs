//! The data relations dialog: fields that hold ids of documents in another collection.

use super::*;

impl App {
    pub(super) fn relations_dialog(&mut self, ui: &mut egui::Ui, conn: &mut u64, db: &mut String, keep: &mut bool) {
        ui.set_max_width(860.0);
        ui.set_width(820.0);
        let colls: Vec<String> = self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.clone()).unwrap_or_default();
        dialog_title(ui, format!("Data relations in {db}"));
        ui.label(RichText::new("A field that holds ids of another collection's documents. Its preview field is shown next to the ids, and \"Go to referenced document\" opens them. Add them by analyzing, or by right-clicking an id and choosing that action.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(6.0);
        let key = format!("{conn}/{db}");
        let before = self.prefs.relations.get(&key).cloned().unwrap_or_default();
        let list = self.prefs.relations.entry(key.clone()).or_default();
        if list.is_empty() {
            ui.label(RichText::new("None yet.").color(FAINT()));
        }
        let mut remove = None;
        egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("rels").num_columns(4).striped(true).spacing([12.0, 8.0]).min_col_width(60.0).show(ui, |ui| {
                caption(ui, "FIELD");
                caption(ui, "POINTS TO COLLECTION");
                caption(ui, "PREVIEW FIELD");
                ui.end_row();
                for (i, r) in list.iter_mut().enumerate() {
                    let name = format!("{}.{}", r.coll, r.path);
                    ui.allocate_ui_with_layout(egui::vec2(260.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_width(260.0);
                        ui.add(egui::Label::new(RichText::new(&name).monospace().color(KEY())).truncate()).on_hover_text(&name);
                    });
                    egui::ComboBox::from_id_salt(("rel-target", i)).width(250.0).selected_text(&r.target).show_ui(ui, |ui| {
                        menu_look(ui);
                        for c in &colls {
                            ui.selectable_value(&mut r.target, c.clone(), c);
                        }
                    });
                    ui.add_sized([180.0, BUTTON_H], egui::TextEdit::singleline(&mut r.preview).hint_text("_id"));
                    if danger(ui, "Remove").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(i) = remove {
            list.remove(i);
        }
        if *list != before {
            self.save_prefs();
        }
        ui.add_space(6.0);
        let mut analyze = None;
        ui.horizontal(|ui| {
            if ui.button("Analyze all collections").on_hover_text("Samples the 10 newest documents of each collection and looks their ids up in the others").clicked() {
                analyze = Some(String::new());
            }
            let cur = self.t().filter(|t| t.sel.conn == *conn && t.sel.db == *db).map(|t| t.sel.coll.clone());
            if let Some(c) = cur {
                if ui.button(format!("Analyze {c}")).clicked() {
                    analyze = Some(c);
                }
            }
        });
        let mut note: Option<db::Res<String>> = None;
        let mut changed = false;
        ui.horizontal(|ui| {
            let list = self.prefs.relations.entry(key.clone()).or_default();
            if ui.add_enabled(!list.is_empty(), egui::Button::new("Export…")).clicked() {
                if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{db}-data-relations.json")).save_file() {
                    match std::fs::write(&path, relations_export(db, list)) {
                        Ok(()) => note = Some(Ok(format!("Exported {} data relations", list.len()))),
                        Err(e) => note = Some(Err(format!("Failed to export relations: {e}"))),
                    }
                }
            }
            if ui.button("Import…").on_hover_text("A file exported here or from Mingo; relations already present are skipped").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                    match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| relations_import(&t)) {
                        Ok(new) => {
                            let mut added = 0;
                            for r in new {
                                if !list.iter().any(|x| x.coll == r.coll && x.path == r.path && x.target == r.target) {
                                    list.push(r);
                                    added += 1;
                                }
                            }
                            changed = true;
                            note = Some(Ok(format!("Imported {added} data relations")));
                        }
                        Err(e) => note = Some(Err(format!("Failed to import relations: {e}"))),
                    }
                }
            }
            let n = self.prefs.relations.get(&key).map_or(0, Vec::len);
            ui.add_enabled_ui(n > 0, |ui| {
                ui.menu_button(RichText::new("Remove all…").color(ALERT()), |ui| {
                    menu_look(ui);
                    if danger(ui, format!("Remove all {n} relations of {db}")).clicked() {
                        self.prefs.relations.remove(&key);
                        changed = true;
                        ui.close();
                    }
                });
            });
        });
        if changed {
            self.save_prefs();
        }
        match note {
            Some(Ok(m)) => self.ok(m),
            Some(Err(e)) => self.err(e),
            None => {}
        }
        if let Some(c) = analyze {
            match self.conn(*conn).and_then(|x| x.client.clone()) {
                Some(client) => self.analyze_relations(*conn, client, db.clone(), c),
                None => self.err("Connect first."),
            }
        }
        if done(ui) {
            *keep = false;
            let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == *conn && t.sel.db == *db).map(|t| t.req).collect();
            for r in reqs {
                if let Some(t) = self.tab_by_req(r) {
                    t.related.clear();
                }
                self.load_related(r);
            }
        }
    }
}
