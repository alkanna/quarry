//! The schema view.

use super::*;

impl App {
    pub(super) fn schema_view(&mut self, ui: &mut egui::Ui) {
        let mut analyze = false;
        let mut acts: Vec<(String, u8)> = Vec::new();
        let key = self.tabs[self.cur].coll_key();
        let maps = self.prefs.schema_maps.get(&key).cloned().unwrap_or_default();
        let sel = &self.tabs[self.cur].sel;
        // Views refuse a $natural sort, so they only offer the first documents.
        let view = self.views.has(sel.conn, &sel.db, &sel.coll);
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
                crate::ui::vim::wrap(ui, id, &mut s.vtext, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().hint_text("{ $jsonSchema: { bsonType: \"object\", required: [...] } }").desired_width(f32::INFINITY).desired_rows(8).layouter(&mut code_layout).show(ui)));
            });
            ui.horizontal(|ui| {
                apply = primary(ui, "Apply", PRIMARY()).on_hover_text("Runs collMod on this collection").clicked();
                generate = ui.add_enabled(!s.stats.is_empty(), egui::Button::new("Generate from analysis")).on_hover_text("Built from the sampled documents; review it before applying").clicked();
                reload = ui.button("Reload").clicked();
            });
        });
        ui.add_space(6.0);
        // The panel takes the place of the field list; the toolbar's buttons below still run.
        if t.schema.err.is_some() {
            self.failed_panel(ui, View::Schema);
        }
        let t = &mut self.tabs[self.cur];
        if t.schema.err.is_some() {
        } else if let Some(ms) = t.schema.ms {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} fields in {} documents  ·  {ms} ms", t.schema.stats.len(), group(&t.schema.docs.to_string()))).color(MUTED()));
                // Fields followed by a deeper row are the ones that fold.
                let s = &mut t.schema;
                let parents: Vec<String> = s.stats.windows(2).filter(|w| w[1].depth > w[0].depth).map(|w| w[0].path.clone()).collect();
                if !parents.is_empty() {
                    ui.add_space(12.0);
                    if quiet(ui, "Expand all").clicked() {
                        s.closed.clear();
                    }
                    if quiet(ui, "Collapse all").clicked() {
                        s.closed.extend(parents);
                    }
                }
            });
        }
        let n = t.schema.docs.max(1) as f32;
        let mut toggle = None;
        egui::ScrollArea::both().id_salt("schema").auto_shrink(false).show(ui, |ui| {
            if t.schema.err.is_some() {
                return;
            }
            egui::Grid::new("schema-grid").num_columns(4).striped(true).spacing([18.0, 6.0]).min_col_width(60.0).show(ui, |ui| {
                for h in ["FIELD", "PRESENT IN", "TYPES", "EXAMPLES"] {
                    caption(ui, h);
                }
                ui.end_row();
                // Stats come as a tree, so a closed field's children are the rows right after it.
                // The depth of a closed field: deeper rows after it are its children.
                let mut hidden: Option<usize> = None;
                for (i, f) in t.schema.stats.iter().enumerate() {
                    if hidden.is_some_and(|d| f.depth > d) {
                        continue;
                    }
                    let open = !t.schema.closed.contains(&f.path);
                    hidden = (!open).then_some(f.depth);
                    let parent = t.schema.stats.get(i + 1).is_some_and(|n| n.depth > f.depth);
                    let name = f.path.rsplit('.').next().unwrap_or(&f.path);
                    let r = ui.horizontal(|ui| {
                        ui.add_space(f.depth as f32 * 16.0);
                        let (rect, arrow) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), if parent { Sense::click() } else { Sense::hover() });
                        if parent {
                            chevron(ui.painter(), rect.center(), open, if arrow.hovered() { TEXT() } else { FAINT() });
                        }
                        let (color, tip) = if name == db::DYNAMIC_KEY { (MUTED(), "Keys that are data rather than names (ids, numbers, dates, keys sharing one pattern, varying keys holding alike objects, or keys of an object marked as a map), merged into one row") } else { (BRIGHT(), f.path.as_str()) };
                        let label = ui.add(egui::Label::new(RichText::new(name).monospace().color(color)).sense(Sense::click())).on_hover_text(tip);
                        if parent && (arrow.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() || label.clicked()) {
                            toggle = Some(f.path.clone());
                        }
                    });
                    // Merged map keys have no single path to filter on or show as a column.
                    if !f.dynamic {
                        // The row only senses hover, so a plain context_menu never sees the right click.
                        r.response.container_context_menu(|ui| {
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
                            // Some maps, like labels keyed by name, look like fields to every guess.
                            if f.types.iter().any(|(t, _)| *t == "object") {
                                ui.separator();
                                let label = if maps.contains(&f.path) { "Show its keys as fields" } else { "Treat its keys as map entries" };
                                if ui.button(label).on_hover_text("Saved for this collection; analyzes again").clicked() {
                                    acts.push((f.path.clone(), 3));
                                }
                            }
                        });
                    }
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
                    let cmd = doc! { "collMod": &t.sel.coll, "validator": v, "validationLevel": level, "validationAction": if t.schema.vwarn { "warn" } else { t.schema.vaction.as_deref().unwrap_or("error") } };
                    self.modal = Some(Modal::Typed { title: format!("Change the validator of {}? Writes that fail it may be rejected.", t.sel.coll), expect: t.sel.coll.clone(), typed: String::new(), action: Danger::Validator(cmd) });
                }
                Err(e) => self.err(e),
            }
        }
        if !self.tabs[self.cur].schema.vloaded && let Some(target) = self.target() {
            self.req += 1;
            let (req, coll) = (self.req, target.coll.clone());
            let s = &mut self.tabs[self.cur].schema;
            (s.vloaded, s.vreq) = (true, req);
            self.worker.command(target.client, target.db, doc! { "listCollections": 1, "filter": { "name": coll } }, &format!("validator:{req}"));
        }
        if let Some(p) = toggle {
            let closed = &mut self.tabs[self.cur].schema.closed;
            if !closed.remove(&p) {
                closed.insert(p);
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
                3 => {
                    let list = self.prefs.schema_maps.entry(key.clone()).or_default();
                    match list.iter().position(|p| *p == path) {
                        Some(i) => _ = list.remove(i),
                        None => list.push(path),
                    }
                    if list.is_empty() {
                        self.prefs.schema_maps.remove(&key);
                    }
                    self.save_prefs();
                    analyze = true;
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
            // Read again: a toggle above changed the saved list after `maps` was taken.
            let maps = self.prefs.schema_maps.get(&key).cloned().unwrap_or_default();
            if let Some(target) = self.target() {
                self.req += 1;
                let req = self.req;
                let t = &mut self.tabs[self.cur];
                t.schema.req = req;
                t.schema.running = true;
                t.schema.err = None;
                self.worker.schema(req, target, t.query.scope(), t.schema.limit, t.schema.last, maps.clone());
            } else if self.demo {
                let t = &mut self.tabs[self.cur];
                t.schema.stats = db::analyze_schema(&t.decoded_docs(), &maps);
                (t.schema.docs, t.schema.ms) = (t.docs.len(), Some(3));
            }
        }
    }
}
