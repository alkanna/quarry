//! Comparing and syncing two collections.

use super::*;
use quarry_core::action::{CmpA, MongoA};

impl App {
    /// Compares this tab's collection with another one, by `_id`, and copies differences over.
    pub(super) fn compare_view(&mut self, ui: &mut egui::Ui) {
        let i = self.core.cur;
        let src = self.core.tabs[i].sel.clone();
        let tab = self.core.tabs[i].uid;
        if self.core.tabs[i].compare.conn == 0 {
            let c = &mut self.core.tabs[i].compare;
            (c.conn, c.db) = (src.conn, src.db.clone());
        }
        self.core.compare_follow();
        let conns: Vec<(u64, String)> = self.core.conns.iter().filter(|c| c.client.is_some() || self.core.demo).map(|c| (c.meta.id, c.meta.name.clone())).collect();
        let dbs = self.conn(self.core.tabs[i].compare.conn).map(|c| c.dbs.clone()).unwrap_or_default();
        let mut acts = Vec::new();
        let failed = self.core.tabs[i].failed.iter().any(|f| f.view == View::Compare);
        let c = &mut self.core.tabs[i].compare;
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
                        acts.push(CmpA::Conn { tab, conn: *id });
                    }
                }
            });
            egui::ComboBox::from_id_salt("cmp-db").selected_text(if c.db.is_empty() { "database" } else { &c.db }).show_ui(ui, |ui| {
                menu_look(ui);
                for (d, _) in &dbs {
                    if ui.selectable_label(c.db == *d, d).clicked() {
                        acts.push(CmpA::Db { tab, db: d.clone() });
                    }
                }
            });
            let colls = dbs.iter().find(|d| d.0 == c.db).and_then(|d| d.1.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("cmp-coll").selected_text(if c.coll.is_empty() { "collection" } else { &c.coll }).show_ui(ui, |ui| {
                menu_look(ui);
                for n in &colls {
                    if ui.selectable_label(c.coll == *n, n).clicked() && c.coll != *n {
                        acts.push(CmpA::Coll { tab, coll: n.clone() });
                    }
                }
            });
            let same = c.conn == src.conn && c.db == src.db && c.coll == src.coll;
            let ok = !c.coll.is_empty() && !same && !c.running;
            if ui.add_enabled(ok, primary_button(ui, "Compare", PRIMARY())).on_disabled_hover_text(if same { "Pick a different collection" } else { "Pick a target collection" }).clicked() {
                acts.push(CmpA::Run { tab });
            }
            if c.running {
                spin(ui);
            }
            });
        });
        if let Some(r) = &c.result {
            ui.add_space(8.0);
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
                        acts.push(CmpA::List { tab, list: l });
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (mut insert, mut replace, mut delete) = (c.insert, c.replace, c.delete);
                ui.checkbox(&mut insert, format!("Copy the {} only in source", r.only_src.len()));
                ui.checkbox(&mut replace, format!("Overwrite the {} different", r.changed.len()));
                let col = if delete { ALERT() } else { TEXT() };
                ui.checkbox(&mut delete, RichText::new(format!("Delete the {} only in target", r.only_dst.len())).color(col));
                if (insert, replace, delete) != (c.insert, c.replace, c.delete) {
                    acts.push(CmpA::Opts { tab, insert, replace, delete });
                }
                let any = (insert && !r.only_src.is_empty()) || (replace && !r.changed.is_empty()) || (delete && !r.only_dst.is_empty());
                if ui.add_enabled(any, primary_button(ui, "Sync to target…", WARNING())).clicked() {
                    acts.push(CmpA::Sync { tab });
                }
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
                    acts.push(CmpA::Page { tab, page: p });
                }
            });
            ui.add_space(4.0);
        } else if !c.running && !failed {
            ui.add_space(20.0);
            ui.label(RichText::new("Pick a target and press Compare. Nothing is written until you sync and type the target's name.").color(FAINT()));
        }
        if failed && self.core.tabs[i].compare.result.is_none() && !self.core.tabs[i].compare.running {
            self.failed_panel(ui, View::Compare);
        }
        if self.core.tabs[i].compare.result.is_some() {
            // The same table as documents, reading the list where the comparison keeps it.
            self.results(ui, true);
        }
        for a in acts {
            self.exec(Action::Mongo(MongoA::Compare(a)));
        }
    }
}
