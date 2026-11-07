//! The indexes view.

use super::*;

impl App {
    pub(crate) fn indexes_view(&mut self, ui: &mut egui::Ui) {
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, "NEW INDEX");
                ui.add(egui::TextEdit::singleline(&mut self.idx_keys).font(egui::TextStyle::Monospace).hint_text("{ email: 1 }").desired_width(280.0));
                ui.checkbox(&mut self.idx_unique, "unique");
                if ui.button("Create").clicked() {
                    match db::parse_doc(&self.idx_keys) {
                        Ok(k) if k.is_empty() => self.err("Give at least one key, like { email: 1 }"),
                        Ok(k) => {
                            if let Some((_, target)) = self.writable_target() {
                                self.worker.create_index(target, k, self.idx_unique);
                            }
                        }
                        Err(e) => self.err(e),
                    }
                }
            });
        });
        ui.add_space(8.0);
        let mut drop = None;
        let mut hide = None;
        let t = self.t().unwrap();
        let key = format!("{}.{}", t.sel.db, t.sel.coll);
        let info = self.idx_info.as_ref().filter(|(k, _)| *k == key).map(|(_, i)| i);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        let (mut create, mut profiling) = (None, false);
        egui::ScrollArea::both().id_salt("indexes").auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("idx").striped(true).num_columns(6).spacing([24.0, 8.0]).show(ui, |ui| {
                for h in ["NAME", "KEYS", "OPTIONS", "USAGE", "SIZE", ""] {
                    caption(ui, h);
                }
                ui.end_row();
                for ix in &t.indexes {
                    let name = ix.get_str("name").unwrap_or_default().to_string();
                    ui.label(RichText::new(&name).monospace().color(BRIGHT()));
                    ui.label(RichText::new(ix.get("keys").map(db::compact).unwrap_or_default()).monospace().color(C_STRING()));
                    let mut opts = ix.clone();
                    opts.remove("name");
                    opts.remove("keys");
                    ui.label(RichText::new(if opts.is_empty() { String::new() } else { db::compact(&Bson::Document(opts)) }).monospace().color(MUTED()));
                    let stat = info.and_then(|i| i.stats.iter().find(|s| s.name == name));
                    ui.horizontal(|ui| {
                        let Some(s) = stat else { return };
                        // A counter that restarted recently cannot prove an index is unused.
                        let recent = s.since.is_some_and(|t| now - t < 7 * 24 * 3600 * 1000);
                        ui.label(RichText::new(format!("{} ops{}", group(&s.ops.to_string()), if s.ops == 0 && recent { ", since restart" } else { "" })).monospace().color(MUTED()));
                        if name != "_id_" && db::is_unused(s.ops, s.since, now) {
                            badge(ui, "unused", WARNING());
                        }
                        if s.hidden {
                            badge(ui, "hidden", PRIMARY());
                        }
                    });
                    ui.label(RichText::new(stat.and_then(|s| s.size).map(human_bytes).unwrap_or_default()).monospace().color(MUTED()));
                    ui.horizontal(|ui| {
                        if name != "_id_" {
                            let hidden = stat.is_some_and(|s| s.hidden);
                            if ui.button(if hidden { "Unhide" } else { "Hide" }).clicked() {
                                hide = Some((name.clone(), !hidden));
                            }
                            if danger(ui, "Drop").clicked() {
                                drop = Some(name);
                            }
                        }
                    });
                    ui.end_row();
                }
            });
            if let Some(i) = info {
                ui.add_space(12.0);
                caption(ui, "SUGGESTIONS");
                // Skip queries an existing index already serves from its leading keys.
                let covered = |sug: &Document| t.indexes.iter().any(|ix| ix.get_document("keys").is_ok_and(|k| k.keys().zip(sug.keys()).filter(|(a, b)| a == b).count() == sug.len()));
                let shapes: Vec<_> = i.shapes.iter().filter(|s| !covered(&s.suggestion)).take(8).collect();
                if i.level == 0 && shapes.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("The profiler is off, so slow queries are not recorded.").color(MUTED()));
                        profiling = ui.button("Enable profiling (slow ops >= 100 ms)").clicked();
                    });
                } else if shapes.is_empty() {
                    ui.label(RichText::new(format!("No unindexed or slow queries recorded (profiler level {}, slow >= {} ms).", i.level, i.slowms)).color(MUTED()));
                }
                for s in shapes {
                    ui.horizontal(|ui| {
                        let sort = if s.sort.is_empty() { String::new() } else { format!("  sort {}", db::compact(&Bson::Document(s.sort.clone()))) };
                        ui.label(RichText::new(format!("{}x  avg {:.0} ms  find {}{sort}", s.count, s.avg_ms, db::compact(&Bson::Document(s.filter.clone())))).monospace().color(MUTED()));
                        ui.label(RichText::new(db::compact(&Bson::Document(s.suggestion.clone()))).monospace().color(C_STRING()));
                        if ui.button("Create").clicked() {
                            create = Some(s.suggestion.clone());
                        }
                    });
                }
            }
        });
        if let Some(k) = create {
            self.idx_keys = db::shell_text(&Bson::Document(k));
        }
        if let Some((name, hidden)) = hide {
            if let Some((conn, target)) = self.writable_target() {
                let done = format!("{} index {name}", if hidden { "Hid" } else { "Unhid" });
                self.worker.db_change(conn, target.client, target.db, doc! { "collMod": target.coll, "index": { "name": name, "hidden": hidden } }, done);
            }
        }
        if profiling {
            if let Some((conn, target)) = self.writable_target() {
                self.worker.db_change(conn, target.client, target.db, doc! { "profile": 1, "slowms": 100 }, "Profiling slow operations".into());
            }
        }
        if let Some(name) = drop {
            self.modal = Some(Modal::Typed {
                title: format!("Drop the index {name}? Queries that rely on it may become slow."),
                expect: name.clone(),
                typed: String::new(),
                action: Danger::DropIndex(name),
            });
        }
    }
}
