//! The schema view.

use super::*;

impl App {
    pub(super) fn schema_view(&mut self, ui: &mut egui::Ui) {
        let mut analyze = false;
        let mut acts: Vec<(String, u8)> = Vec::new();
        let sel = &self.tabs[self.cur].sel;
        // Views refuse a $natural sort, so they only offer the first documents.
        let view = self.views.contains(&(sel.conn, sel.db.clone(), sel.coll.clone()));
        let t = &mut self.tabs[self.cur];
        if view {
            t.schema.last = false;
        }
        // Analyze once when the view opens; the limit keeps it light on big collections.
        if t.schema.stats.is_empty() && !t.schema.running && t.schema.ms.is_none() && t.schema.err.is_none() {
            analyze = true;
        }
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, "SCHEMA  ·  fields of the documents matching the current query");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    analyze |= primary(ui, "Analyze", PRIMARY()).clicked();
                    let label = |(n, last): (i64, bool)| if n == 0 { "all documents".to_string() } else { format!("{} {}", if last { "last" } else { "first" }, group(&n.to_string())) };
                    let mut pick = (t.schema.limit, t.schema.last);
                    egui::ComboBox::from_id_salt("schema-limit").selected_text(label(pick)).show_ui(ui, |ui| {
                        menu_look(ui);
                        let mut group = |ui: &mut egui::Ui, last: bool| {
                            for n in [100, 1000, 10_000, 100_000].into_iter().filter(|&n| !last || n >= 1000) {
                                ui.selectable_value(&mut pick, (n, last), label((n, last)));
                            }
                            ui.separator();
                        };
                        group(ui, false);
                        if !view {
                            group(ui, true);
                        }
                        ui.selectable_value(&mut pick, (0, false), label((0, false)));
                    });
                    (t.schema.limit, t.schema.last) = pick;
                    if t.schema.running {
                        spin(ui);
                    }
                });
            });
            let filter = if t.query.filter.is_empty() { "{} (all documents)".to_string() } else { db::shell_text(&Bson::Document(t.query.filter.clone())) };
            ui.label(RichText::new(format!("Query: {filter}")).monospace().size(TXT_SMALL).color(FAINT()));
        });
        ui.add_space(6.0);
        let (mut apply, mut generate, mut reload) = (false, false, false);
        // QUARRY_TEST_VALIDATOR opens it in the demo picture.
        egui::CollapsingHeader::new(caption_text("VALIDATOR")).id_salt("schema-validator").default_open(std::env::var("QUARRY_TEST_VALIDATOR").is_ok()).show(ui, |ui| {
            let s = &mut t.schema;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Level").color(MUTED()));
                segmented(ui, &mut s.vlevel, &[(0, "off"), (1, "moderate"), (2, "strict")]);
                ui.add_space(12.0);
                ui.label(RichText::new("Action").color(MUTED()));
                segmented(ui, &mut s.vwarn, &[(false, "error"), (true, "warn")]);
            });
            let id = ui.make_persistent_id("validator-editor");
            // A generated validator can run to hundreds of lines; scroll inside a capped box so the
            // Apply buttons and the field list stay in the window.
            let max_h = (ui.ctx().content_rect().height() * 0.4).clamp(160.0, 420.0);
            egui::ScrollArea::vertical().id_salt("validator-scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                with_autopair(ui, id, &mut s.vtext, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().hint_text("{ $jsonSchema: { bsonType: \"object\", required: [...] } }").desired_width(f32::INFINITY).desired_rows(8).layouter(&mut code_layout).show(ui));
            });
            ui.horizontal(|ui| {
                apply = primary(ui, "Apply", PRIMARY()).on_hover_text("Runs collMod on this collection").clicked();
                generate = ui.add_enabled(!s.stats.is_empty(), egui::Button::new("Generate from analysis")).on_hover_text("Built from the sampled documents; review it before applying").clicked();
                reload = ui.button("⟳ Reload").clicked();
            });
        });
        ui.add_space(6.0);
        if let Some(e) = &t.schema.err {
            ui.label(RichText::new(format!("Analysis failed: {e}")).color(ALERT()));
        } else if let Some(ms) = t.schema.ms {
            ui.label(RichText::new(format!("{} fields in {} documents  ·  {ms} ms", t.schema.stats.len(), group(&t.schema.docs.to_string()))).color(MUTED()));
        }
        let n = t.schema.docs.max(1) as f32;
        egui::ScrollArea::vertical().id_salt("schema").auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("schema-grid").num_columns(4).striped(true).spacing([18.0, 6.0]).min_col_width(60.0).show(ui, |ui| {
                for h in ["FIELD", "PRESENT IN", "TYPES", "EXAMPLES"] {
                    caption(ui, h);
                }
                ui.end_row();
                for f in &t.schema.stats {
                    let name = f.path.rsplit('.').next().unwrap_or(&f.path);
                    let r = ui.horizontal(|ui| {
                        ui.add_space(f.depth as f32 * 16.0);
                        ui.label(RichText::new(name).monospace().color(BRIGHT())).on_hover_text(&f.path)
                    });
                    r.response.context_menu(|ui| {
                        menu_look(ui);
                        ui.label(RichText::new(format!("Field: {}", f.path)).size(TXT_SMALL).color(MUTED()));
                        if ui.button("Add as column").clicked() {
                            acts.push((f.path.clone(), 0));
                        }
                        if ui.button("Filter: has this field").clicked() {
                            acts.push((f.path.clone(), 1));
                        }
                        if ui.button("Filter: missing this field").clicked() {
                            acts.push((f.path.clone(), 2));
                        }
                    });
                    let share = f.docs as f32 / n;
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(rect, R_SM, HOVER());
                        let mut fill = rect;
                        fill.set_width(rect.width() * share);
                        ui.painter().rect_filled(fill, R_SM, if share >= 0.999 { SUCCESS() } else { PRIMARY() });
                        ui.label(RichText::new(format!("{:.0}%", share * 100.0)).size(TXT_SMALL).color(TEXT()));
                    });
                    ui.horizontal(|ui| {
                        let total: usize = f.types.iter().map(|t| t.1).sum::<usize>().max(1);
                        for (tn, c) in &f.types {
                            let col = color_of(&match *tn {
                                "string" => Bson::String(String::new()),
                                "int" | "long" | "double" | "decimal" => Bson::Int32(0),
                                "bool" => Bson::Boolean(true),
                                "date" | "timestamp" => Bson::DateTime(mongodb::bson::DateTime::from_millis(0)),
                                "null" => Bson::Null,
                                "objectId" => Bson::ObjectId(ObjectId::new()),
                                _ => Bson::Document(Document::new()),
                            });
                            let pct = *c as f32 * 100.0 / total as f32;
                            let label = if f.types.len() > 1 { format!("{tn} {pct:.0}%") } else { tn.to_string() };
                            ui.label(RichText::new(label).monospace().size(TXT_SMALL).color(col));
                        }
                    });
                    ui.label(RichText::new(f.examples.join("   ")).monospace().size(TXT_SMALL).color(FAINT()));
                    ui.end_row();
                }
            });
        });
        if generate {
            let s = &mut self.tabs[self.cur].schema;
            s.vtext = db::to_edit_json(&db::schema_from_stats(&s.stats, s.docs));
        }
        if reload {
            self.tabs[self.cur].schema.vloaded = false;
        }
        if apply {
            let t = &self.tabs[self.cur];
            match db::parse_doc(&t.schema.vtext) {
                Ok(v) => {
                    let level = ["off", "moderate", "strict"][t.schema.vlevel as usize];
                    let cmd = doc! { "collMod": &t.sel.coll, "validator": v, "validationLevel": level, "validationAction": if t.schema.vwarn { "warn" } else { "error" } };
                    self.modal = Some(Modal::Typed { title: format!("Change the validator of {}? Writes that fail it may be rejected.", t.sel.coll), expect: t.sel.coll.clone(), typed: String::new(), action: Danger::Validator(cmd) });
                }
                Err(e) => self.err(e),
            }
        }
        if !self.tabs[self.cur].schema.vloaded {
            if let Some(target) = self.target() {
                self.req += 1;
                let (req, coll) = (self.req, target.coll.clone());
                let s = &mut self.tabs[self.cur].schema;
                (s.vloaded, s.vreq) = (true, req);
                self.worker.command(target.client, target.db, doc! { "listCollections": 1, "filter": { "name": coll } }, &format!("validator:{req}"));
            }
        }
        for (path, what) in acts {
            match what {
                0 => {
                    let key = self.col_key(false);
                    let fields = self.t().map(|t| t.fields.clone()).unwrap_or_default();
                    self.apply_col_acts(&key, &fields, vec![ColAct::Show(path)]);
                    self.ok("Column added; open Documents to see it");
                }
                _ => {
                    let t = &mut self.tabs[self.cur];
                    t.filter_txt = db::shell_text(&Bson::Document(doc! { path: { "$exists": what == 1 } }));
                    t.view = View::Documents;
                    self.run_query();
                }
            }
        }
        if analyze {
            if let Some(target) = self.target() {
                self.req += 1;
                let req = self.req;
                let t = &mut self.tabs[self.cur];
                t.schema.req = req;
                t.schema.running = true;
                t.schema.err = None;
                self.worker.schema(req, target, t.query.filter.clone(), t.schema.limit, t.schema.last);
            } else if self.demo {
                let t = &mut self.tabs[self.cur];
                t.schema.stats = db::analyze_schema(&t.decoded_docs());
                (t.schema.docs, t.schema.ms) = (t.docs.len(), Some(3));
            }
        }
    }
}
