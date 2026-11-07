//! Comparing and syncing two collections.

use super::*;

impl App {
    /// Compares this tab's collection with another one, by `_id`, and copies differences over.
    pub(super) fn compare_view(&mut self, ui: &mut egui::Ui) {
        let i = self.cur;
        let src = self.tabs[i].sel.clone();
        if self.tabs[i].compare.conn == 0 {
            let c = &mut self.tabs[i].compare;
            (c.conn, c.db) = (src.conn, src.db.clone());
        }
        let conns: Vec<(u64, String)> = self.conns.iter().filter(|c| c.client.is_some() || self.demo).map(|c| (c.meta.id, c.meta.name.clone())).collect();
        let dbs = self.conn(self.tabs[i].compare.conn).map(|c| c.dbs.clone()).unwrap_or_default();
        let mut load_colls = None;
        let mut run = false;
        let mut sync = false;
        let c = &mut self.tabs[i].compare;
        toolbar(ui, |ui| {
            ui.set_width(ui.available_width());
            caption(ui, format!("COMPARE  ·  {}.{} is the source, documents are matched by _id and field order is ignored", src.db, src.coll));
            ui.horizontal(|ui| {
            ui.label("Target");
            let name = conns.iter().find(|x| x.0 == c.conn).map(|x| x.1.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("cmp-conn").selected_text(name).show_ui(ui, |ui| {
                for (id, n) in &conns {
                    if ui.selectable_label(c.conn == *id, n).clicked() && c.conn != *id {
                        (c.conn, c.db, c.coll) = (*id, String::new(), String::new());
                    }
                }
            });
            egui::ComboBox::from_id_salt("cmp-db").selected_text(if c.db.is_empty() { "database" } else { &c.db }).show_ui(ui, |ui| {
                for (d, colls) in &dbs {
                    if ui.selectable_label(c.db == *d, d).clicked() {
                        c.db = d.clone();
                        c.coll.clear();
                        if colls.is_none() {
                            load_colls = Some(d.clone());
                        }
                    }
                }
            });
            let colls = dbs.iter().find(|d| d.0 == c.db).and_then(|d| d.1.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("cmp-coll").selected_text(if c.coll.is_empty() { "collection" } else { &c.coll }).show_ui(ui, |ui| {
                for n in &colls {
                    ui.selectable_value(&mut c.coll, n.clone(), n);
                }
            });
            let same = c.conn == src.conn && c.db == src.db && c.coll == src.coll;
            let ok = !c.coll.is_empty() && !same && !c.running;
            run = ui.add_enabled(ok, primary_button(ui, "Compare", PRIMARY())).on_disabled_hover_text(if same { "Pick a different collection" } else { "Pick a target collection" }).clicked();
            if c.running {
                spin(ui);
            }
            });
        });
        // Syncing writes the old diff to whatever target is picked, so a diff for another one is dropped.
        if c.ran != (c.conn, c.db.clone(), c.coll.clone()) {
            c.result = None;
        }
        if let Some(r) = &c.result {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (label, n, color) in [("Identical", r.same, MUTED()), ("Only in source", r.only_src.len(), SUCCESS()), ("Only in target", r.only_dst.len(), WARNING()), ("Different", r.changed.len(), C_LINK())] {
                    egui::Frame::new().fill(RAISED()).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.label(RichText::new(group(&n.to_string())).strong().size(TXT_HEADING).color(color));
                        ui.label(RichText::new(label).size(TXT_SMALL).color(MUTED()));
                    });
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height((ui.available_height() - 110.0).max(120.0)).auto_shrink([false, true]).show(ui, |ui| {
                const SHOWN: usize = 200;
                let more = |ui: &mut egui::Ui, n: usize| {
                    if n > SHOWN {
                        ui.label(RichText::new(format!("and {} more", group(&(n - SHOWN).to_string()))).color(FAINT()));
                    }
                };
                egui::CollapsingHeader::new(format!("Different ({})", r.changed.len())).id_salt("cmp-changed").default_open(true).show(ui, |ui| {
                    for (a, b) in r.changed.iter().take(SHOWN) {
                        let d = db::diff(b, a);
                        egui::CollapsingHeader::new(RichText::new(format!("{}   {} field{} differ{}", db::short(a.get("_id").unwrap_or(&Bson::Null)), d.len(), if d.len() == 1 { "" } else { "s" }, if d.len() == 1 { "s" } else { "" })).monospace()).id_salt(("cmp-d", db::compact(a.get("_id").unwrap_or(&Bson::Null)))).show(ui, |ui| {
                            egui::Grid::new(("cmp-g", db::compact(a.get("_id").unwrap_or(&Bson::Null)))).striped(true).show(ui, |ui| {
                                ui.label(RichText::new("Field").color(FAINT()));
                                ui.label(RichText::new("Target now").color(FAINT()));
                                ui.label(RichText::new("Source").color(FAINT()));
                                ui.end_row();
                                for (path, old, new) in d {
                                    let t = |v: &Option<Bson>| v.as_ref().map_or("missing".to_string(), cell_text);
                                    ui.label(RichText::new(path).monospace().color(KEY()));
                                    ui.label(RichText::new(t(&old)).monospace().color(ALERT()));
                                    ui.label(RichText::new(t(&new)).monospace().color(SUCCESS()));
                                    ui.end_row();
                                }
                            });
                        });
                    }
                    more(ui, r.changed.len());
                });
                for (title, list) in [("Only in source", &r.only_src), ("Only in target", &r.only_dst)] {
                    egui::CollapsingHeader::new(format!("{title} ({})", list.len())).id_salt(title).show(ui, |ui| {
                        for d in list.iter().take(SHOWN) {
                            let mut t = db::compact(&Bson::Document(d.clone()));
                            if t.len() > 160 {
                                t = format!("{}…", &t[..t.floor_char_boundary(160)]);
                            }
                            ui.add(egui::Label::new(RichText::new(t).monospace().size(TXT_SMALL).color(TEXT())).truncate());
                        }
                        more(ui, list.len());
                    });
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.insert, format!("Copy the {} only in source", r.only_src.len()));
                ui.checkbox(&mut c.replace, format!("Overwrite the {} different", r.changed.len()));
                let col = if c.delete { ALERT() } else { TEXT() };
                ui.checkbox(&mut c.delete, RichText::new(format!("Delete the {} only in target", r.only_dst.len())).color(col));
                let any = (c.insert && !r.only_src.is_empty()) || (c.replace && !r.changed.is_empty()) || (c.delete && !r.only_dst.is_empty());
                sync = ui.add_enabled(any, primary_button(ui, "Sync to target…", WARNING())).clicked();
            });
        } else if !c.running {
            ui.add_space(20.0);
            ui.label(RichText::new("Pick a target and press Compare. Nothing is written until you sync and type the target's name.").color(FAINT()));
        }
        let (tc, tdb, tcoll) = (c.conn, c.db.clone(), c.coll.clone());
        if let Some(d) = load_colls {
            if let Some(client) = self.conn(tc).and_then(|c| c.client.clone()) {
                self.worker.collections(tc, client, d);
            }
        }
        if run {
            let (Some(a), Some(b)) = (self.conn(src.conn).and_then(|c| c.client.clone()), self.conn(tc).and_then(|c| c.client.clone())) else {
                return self.err(if self.demo { "Offline demo: nothing to compare." } else { "Connect both sides first." });
            };
            self.req += 1;
            let req = self.req;
            let to = self.timeout();
            let c = &mut self.tabs[i].compare;
            (c.req, c.running, c.result, c.ran) = (req, true, None, (tc, tdb.clone(), tcoll.clone()));
            self.worker.compare(req, Target { client: a, db: src.db, coll: src.coll, timeout: to }, Target { client: b, db: tdb, coll: tcoll, timeout: to });
        } else if sync {
            if !self.conn(tc).is_some_and(|c| c.writable) {
                return self.modal = Some(Modal::Unlock(tc, None));
            }
            let c = &self.tabs[i].compare;
            let r = c.result.as_ref().unwrap();
            let n = |on: bool, k: usize| if on { k } else { 0 };
            let title = format!(
                "Write to {tdb}.{tcoll}: copy {}, overwrite {} and delete {} documents? Overwrites skip documents changed since the comparison. This has no Undo.",
                n(c.insert, r.only_src.len()),
                n(c.replace, r.changed.len()),
                n(c.delete, r.only_dst.len())
            );
            self.modal = Some(Modal::Typed { title, expect: tcoll, typed: String::new(), action: Danger::Sync { conn: tc } });
        }
    }
}
