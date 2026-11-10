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
        let failed = self.tabs[i].failed.iter().any(|f| f.view == View::Compare);
        let c = &mut self.tabs[i].compare;
        toolbar(ui, |ui| {
            ui.set_width(ui.available_width());
            caption(ui, format!("COMPARE  ·  {}.{} is the source, documents are matched by _id and field order is ignored", src.db, src.coll));
            ui.horizontal(|ui| {
            ui.label("Target");
            let name = conns.iter().find(|x| x.0 == c.conn).map(|x| x.1.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("cmp-conn").selected_text(name).show_ui(ui, |ui| {
                menu_look(ui);
                for (id, n) in &conns {
                    if ui.selectable_label(c.conn == *id, n).clicked() && c.conn != *id {
                        (c.conn, c.db, c.coll) = (*id, String::new(), String::new());
                    }
                }
            });
            egui::ComboBox::from_id_salt("cmp-db").selected_text(if c.db.is_empty() { "database" } else { &c.db }).show_ui(ui, |ui| {
                menu_look(ui);
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
                menu_look(ui);
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
        // Open rows are kept by position, so whenever the rows under them change they are closed.
        let mut moved = false;
        if c.result.is_some() && c.ran != (c.conn, c.db.clone(), c.coll.clone()) {
            c.set_result(None);
            moved = true;
        }
        if let Some(r) = &c.result {
            ui.add_space(8.0);
            let mut pick = None;
            ui.horizontal(|ui| {
                for (label, n, color, list) in [("Different", r.changed.len(), C_LINK(), Some(CmpList::Changed)), ("Only in source", r.only_src.len(), SUCCESS(), Some(CmpList::OnlySrc)), ("Only in target", r.only_dst.len(), WARNING(), Some(CmpList::OnlyDst)), ("Identical", r.same, MUTED(), None)] {
                    let on = list == Some(c.list);
                    let stroke = Stroke::new(1.0, if on { color } else { Color32::TRANSPARENT });
                    let f = egui::Frame::new().fill(RAISED()).stroke(stroke).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.label(semibold(RichText::new(group(&n.to_string())).size(TXT_HEADING).color(color)));
                        ui.label(RichText::new(label).size(TXT_SMALL).color(if on { BRIGHT() } else { MUTED() }));
                    });
                    // Identical documents are counted, not kept, so there is nothing to list.
                    if let Some(l) = list
                        && f.response.interact(Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                        && l != c.list
                    {
                        pick = Some(l);
                    }
                }
            });
            if let Some(l) = pick {
                (c.list, c.page, moved) = (l, 0, true);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.insert, format!("Copy the {} only in source", r.only_src.len()));
                ui.checkbox(&mut c.replace, format!("Overwrite the {} different", r.changed.len()));
                let col = if c.delete { ALERT() } else { TEXT() };
                ui.checkbox(&mut c.delete, RichText::new(format!("Delete the {} only in target", r.only_dst.len())).color(col));
                let any = (c.insert && !r.only_src.is_empty()) || (c.replace && !r.changed.is_empty()) || (c.delete && !r.only_dst.is_empty());
                sync = ui.add_enabled(any, primary_button(ui, "Sync to target…", WARNING())).clicked();
            });
            let n = c.rows().len();
            let pages = n.div_ceil(PAGE_SIZE as usize).max(1);
            c.page = c.page.min(pages - 1);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let what = if c.list == CmpList::Changed { "differing fields" } else { "documents" };
                ui.label(RichText::new(format!("{} {what}", group(&n.to_string()))).color(BRIGHT()));
                ui.separator();
                if let Some(p) = pager(ui, c.page, pages) {
                    (c.page, moved) = (p, true);
                }
            });
            ui.add_space(4.0);
        } else if !c.running && !failed {
            ui.add_space(20.0);
            ui.label(RichText::new("Pick a target and press Compare. Nothing is written until you sync and type the target's name.").color(FAINT()));
        }
        if moved {
            self.drop_agg_row();
        }
        if failed && self.tabs[i].compare.result.is_none() && !self.tabs[i].compare.running {
            self.failed_panel(ui, View::Compare);
        }
        if self.tabs[i].compare.result.is_some() {
            // The same table as documents, reading the list where the comparison keeps it.
            self.results(ui, true);
        }
        let c = &mut self.tabs[i].compare;
        let (tc, tdb, tcoll) = (c.conn, c.db.clone(), c.coll.clone());
        if let Some(d) = load_colls {
            if let Some(client) = self.conn(tc).and_then(|c| c.client.clone()) {
                self.worker.collections(tc, client, d);
            }
        }
        if run {
            let (Some(a), Some(b)) = (self.conn(src.conn).and_then(|c| c.client.clone()), self.conn(tc).and_then(|c| c.client.clone())) else {
                return self.warn(if self.demo { "Offline demo: nothing to compare." } else { "Connect both sides first." });
            };
            self.req += 1;
            let req = self.req;
            let to = self.timeout();
            self.drop_agg_row();
            let c = &mut self.tabs[i].compare;
            c.set_result(None);
            (c.req, c.running, c.ran) = (req, true, (tc, tdb.clone(), tcoll.clone()));
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
