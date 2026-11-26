//! The data relations dialog: fields that hold ids of documents in another collection.

use super::*;
use quarry_core::action::MongoA;

impl App {
    pub(super) fn relations_dialog(&mut self, ui: &mut egui::Ui, conn: &mut u64, db: &mut String, keep: &mut bool) {
        ui.set_max_width(860.0);
        ui.set_width(820.0);
        let colls: Vec<String> = self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.clone()).unwrap_or_default();
        dialog_head(ui, &Head::new(format!("Data relations in {db}")).sub("Fields that hold ids of another collection's documents").glyph(glyphs::link));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        ui.add(egui::Label::new(RichText::new("Each relation's preview field is shown next to its ids, and \"Go to referenced document\" opens them. Add them by analyzing, or by right-clicking an id and choosing that action.").size(TXT_SMALL).color(MUTED())).wrap());
        ui.add_space(10.0);
        let key = format!("{conn}/{db}");
        let before = self.core.prefs.relations.get(&key).cloned().unwrap_or_default();
        let mut list = before.clone();
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
        ui.add_space(6.0);
        let mut analyze = None;
        ui.horizontal(|ui| {
            if ui.button("Analyze all collections").on_hover_text("Samples the 10 newest documents of each collection and looks their ids up in the others").clicked() {
                analyze = Some(String::new());
            }
            let cur = self.t().filter(|t| t.sel.conn == *conn && t.sel.db == *db).map(|t| t.sel.coll.clone());
            if let Some(c) = cur && ui.button(format!("Analyze {c}")).clicked() {
                analyze = Some(c);
            }
        });
        let mut note: Option<db::Res<String>> = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(!list.is_empty(), egui::Button::new("Export…")).clicked() && let Some(path) = rfd::FileDialog::new().set_file_name(format!("{db}-data-relations.json")).save_file() {
                match std::fs::write(&path, relations_export(db, &list)) {
                    Ok(()) => note = Some(Ok(format!("Exported {} data relations", list.len()))),
                    Err(e) => note = Some(Err(format!("Failed to export relations: {e}"))),
                }
            }
            if ui.button("Import…").on_hover_text("A file exported here or from Mingo; relations already present are skipped").clicked() && let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| relations_import(&t)) {
                    Ok(new) => {
                        let mut added = 0;
                        for r in new {
                            if !list.iter().any(|x| x.coll == r.coll && x.path == r.path && x.target == r.target) {
                                list.push(r);
                                added += 1;
                            }
                        }
                        note = Some(Ok(format!("Imported {added} data relations")));
                    }
                    Err(e) => note = Some(Err(format!("Failed to import relations: {e}"))),
                }
            }
            let n = list.len();
            ui.add_enabled_ui(n > 0, |ui| {
                ui.menu_button(RichText::new("Remove all…").color(ALERT()), |ui| {
                    menu_look(ui);
                    if danger(ui, format!("Remove all {n} relations of {db}")).clicked() {
                        list.clear();
                        ui.close();
                    }
                });
            });
        });
        if list != before {
            self.exec(Action::Mongo(MongoA::Relations { key: key.clone(), list }));
        }
        match note {
            Some(Ok(m)) => self.ok(m),
            Some(Err(e)) => self.err(e),
            None => {}
        }
        if let Some(c) = analyze {
            self.exec(Action::Mongo(MongoA::AnalyzeRelations { conn: *conn, db: db.clone(), coll: c }));
        }
        b.end(outer);
        let ui = outer;
        if dialog_done(ui) {
            *keep = false;
            self.exec(Action::Mongo(MongoA::ReloadRelated { conn: *conn, db: db.clone() }));
        }
    }
}

/// Relations as the original's export file (exportRelations, renderer line 135294), so files
/// move between Quarry and the original.
pub(super) fn relations_export(db: &str, list: &[Relation]) -> String {
    let rel: Vec<serde_json::Value> = list.iter().map(|r| serde_json::json!({ "collection": r.coll, "path": r.path, "referencedCollection": r.target, "previewField": r.preview })).collect();
    let now = mongodb::bson::DateTime::now().try_to_rfc3339_string().unwrap_or_default();
    serde_json::to_string_pretty(&serde_json::json!({ "version": "1.0", "database": db, "exportedAt": now, "relations": rel })).unwrap_or_default()
}

/// Reads an export file; files without `version` and `relations` are refused, like the original
/// (importRelations, 135332). Array markers in the original's paths ("items[].id") are dropped.
pub(super) fn relations_import(text: &str) -> db::Res<Vec<Relation>> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let (Some(_), Some(list)) = (v.get("version"), v.get("relations").and_then(|r| r.as_array())) else {
        return Err("Invalid data relations file format".into());
    };
    let s = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    Ok(list.iter().map(|r| Relation { coll: s(r, "collection"), path: s(r, "path").replace("[]", ""), target: s(r, "referencedCollection"), preview: s(r, "previewField") }).filter(|r| !r.coll.is_empty() && !r.path.is_empty() && !r.target.is_empty()).collect())
}

impl App {
    /// The sidebar's Analyze: the connection's client is looked up again by the action.
    pub(super) fn analyze_relations(&mut self, conn: u64, _client: Client, db: String, coll: String) {
        self.exec(Action::Mongo(MongoA::AnalyzeRelations { conn, db, coll }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_files() {
        let list = vec![Relation { coll: "orders".into(), path: "items.productId".into(), target: "products".into(), preview: "title".into() }];
        assert!(relations_import(&relations_export("shop", &list)).unwrap() == list);
        let mingo = r#"{"version":"1.0","database":"shop","relations":[{"collection":"orders","path":"items[].productId","referencedCollection":"products","previewField":"title"}]}"#;
        assert!(relations_import(mingo).unwrap() == list);
        assert!(relations_import(r#"{"relations":[]}"#).is_err());
    }
}
