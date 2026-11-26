//! The indexes view: one row per index in the shared results table, profiler suggestions, and
//! the dialog that creates an index with all its options.

use super::*;
pub(crate) use quarry_core::state::indexes::*;

use quarry_core::action::indexes::{stats_note, IdxA};
use quarry_core::action::Action;

/// What a row's menu asks for.
pub(super) enum IdxAct {
    Hide(bool),
    Drop,
    CopyCmd,
    CopyKeys,
}

/// The menu of one index row.
pub(super) fn index_menu(ui: &mut egui::Ui, row: &Document) -> Option<IdxAct> {
    menu_look(ui);
    let name = row.get_str("name").unwrap_or_default();
    let hidden = row.get_document("options").is_ok_and(|o| o.get_bool("hidden") == Ok(true));
    ui.label(RichText::new(format!("Index {name}")).size(TXT_CAPTION).color(FAINT()));
    let mut act = None;
    if ui.button("Copy as createIndex(…)").clicked() {
        act = Some(IdxAct::CopyCmd);
    }
    if ui.button("Copy keys").clicked() {
        act = Some(IdxAct::CopyKeys);
    }
    if name != "_id_" {
        ui.separator();
        if ui.button(if hidden { "Unhide" } else { "Hide" }).on_hover_text("The query planner ignores a hidden index, so you can see what dropping it would do.").clicked() {
            act = Some(IdxAct::Hide(!hidden));
        }
        if danger(ui, "Drop…").clicked() {
            act = Some(IdxAct::Drop);
        }
    }
    act
}

impl App {
    pub(super) fn indexes_view(&mut self, ui: &mut egui::Ui) {
        let mut new = false;
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                new = primary(ui, "New index…", PRIMARY()).clicked();
                let n = self.t().map_or(0, |t| t.idx_rows.len());
                ui.label(RichText::new(format!("{n} index{}", if n == 1 { "" } else { "es" })).color(MUTED()));
            });
        });
        if new {
            self.open_create_index(None);
        }
        self.index_suggestions(ui);
        ui.add_space(6.0);
        self.results(ui, true);
    }

    /// Unindexed or slow queries the profiler saw, each with an index that would serve it.
    fn index_suggestions(&mut self, ui: &mut egui::Ui) {
        let t = self.t().unwrap();
        let key = t.sel.key();
        let Some(i) = self.core.idx_info.as_ref().filter(|(k, _)| *k == key).map(|(_, i)| i) else { return };
        if let Some(e) = &i.stats_err {
            ui.add_space(6.0);
            ui.label(RichText::new(stats_note(e)).color(MUTED())).on_hover_text(e);
        }
        // Skip queries an existing index already serves from its leading keys.
        let covered = |sug: &Document| t.indexes.iter().any(|ix| ix.get_document("keys").is_ok_and(|k| k.keys().zip(sug.keys()).filter(|(a, b)| a == b).count() == sug.len()));
        let shapes: Vec<_> = i.shapes.iter().filter(|s| !covered(&s.suggestion)).take(8).collect();
        let (mut create, mut profiling) = (None, false);
        ui.add_space(6.0);
        if i.level == 0 && shapes.is_empty() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("The profiler is off, so slow queries are not recorded.").color(MUTED()));
                profiling = ui.button("Enable profiling (slow ops >= 100 ms)").clicked();
            });
        } else if shapes.is_empty() {
            ui.label(RichText::new(format!("No unindexed or slow queries recorded (profiler level {}, slow >= {} ms).", i.level, i.slowms)).color(MUTED()));
        } else {
            caption(ui, "SUGGESTIONS");
        }
        for s in shapes {
            ui.horizontal(|ui| {
                let sort = if s.sort.is_empty() { String::new() } else { format!("  sort {}", db::compact(&Bson::Document(s.sort.clone()))) };
                ui.label(RichText::new(format!("{}x  avg {:.0} ms  find {}{sort}", s.count, s.avg_ms, db::compact(&Bson::Document(s.filter.clone())))).monospace().color(MUTED()));
                ui.label(RichText::new(db::compact(&Bson::Document(s.suggestion.clone()))).monospace().color(C_STRING()));
                if ui.button("Create…").clicked() {
                    create = Some(s.suggestion.clone());
                }
            });
        }
        if let Some(k) = create {
            self.open_create_index(Some(k));
        }
        if profiling {
            let tab = self.t().unwrap().uid;
            self.exec(Action::Indexes(IdxA::Profile { tab }));
        }
    }

    pub(super) fn open_create_index(&mut self, keys: Option<Document>) {
        let keys = keys.map_or_else(|| "{  }".into(), |k| db::shell_text(&Bson::Document(k)));
        self.modal = Some(Modal::CreateIndex(Box::new(IndexForm { keys, strength: 3, ..Default::default() })));
    }

    /// The New index dialog; returns whether it stays open.
    pub(super) fn create_index_dialog(&mut self, ui: &mut egui::Ui, f: &mut IndexForm) -> bool {
        if self.t().is_none() {
            return false;
        }
        let mut ac = std::mem::take(&mut self.ui.ac);
        let t = self.t().unwrap();
        let coll = t.sel.coll.clone();
        dialog_head(ui, &Head::new(format!("New index on {coll}")).glyph(glyphs::list));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        caption(ui, "KEYS");
        let id = egui::Id::unique("index-keys");
        ac_field(ui, id, &mut f.keys, &mut ac, &|b, _| suggest(b, FieldKind::Sort, &t.paths, &t.field_info), |ui, text, _| {
            egui::TextEdit::singleline(text).id(id).font(egui::TextStyle::Monospace).hint_text("{ email: 1, createdAt: -1 }").desired_width(440.0).show(ui)
        });
        self.ui.ac = ac;
        ui.label(RichText::new("1 ascending, -1 descending, or \"text\", \"2dsphere\", \"hashed\".").size(TXT_SMALL).color(MUTED()));
        ui.add_space(6.0);
        egui::Grid::new("index-options").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut f.name).font(egui::TextStyle::Monospace).hint_text("generated from the keys").desired_width(280.0));
            ui.end_row();
            ui.label("Options");
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.unique, "unique").on_hover_text("Refuses a second document with the same key values.");
                ui.checkbox(&mut f.sparse, "sparse").on_hover_text("Leaves out documents that lack the indexed field.");
                ui.checkbox(&mut f.hidden, "hidden").on_hover_text("Built and kept up to date, but not used by queries until unhidden.");
            });
            ui.end_row();
            ui.label("Expire after");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.ttl).font(egui::TextStyle::Monospace).hint_text("seconds").desired_width(100.0));
                ui.label(RichText::new("TTL: deletes each document that long after the date in its key field.").size(TXT_SMALL).color(MUTED()));
            });
            ui.end_row();
            ui.label("Partial filter");
            ui.add(egui::TextEdit::singleline(&mut f.partial).font(egui::TextStyle::Monospace).hint_text("{ status: \"active\" }").desired_width(280.0));
            ui.end_row();
            ui.label("Collation");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.locale).font(egui::TextStyle::Monospace).hint_text("locale, like en").desired_width(100.0));
                const STRENGTHS: [&str; 5] = ["1 base letters", "2 and accents", "3 and case", "4 and punctuation", "5 identical"];
                ui.add_enabled_ui(!f.locale.trim().is_empty(), |ui| {
                    egui::ComboBox::from_id_salt("index-strength").selected_text(STRENGTHS[(f.strength.clamp(1, 5) - 1) as usize]).show_ui(ui, |ui| {
                        menu_look(ui);
                        for (i, s) in STRENGTHS.iter().enumerate() {
                            ui.selectable_value(&mut f.strength, i as i32 + 1, *s);
                        }
                    });
                });
            });
            ui.end_row();
        });
        ui.add_space(6.0);
        let spec = f.spec();
        let command = |s: Document| doc! { "createIndexes": &coll, "indexes": [s] };
        match &spec {
            Ok(s) => {
                caption(ui, "COMMAND");
                ui.label(RichText::new(db::shell_text(&Bson::Document(command(s.clone())))).monospace().color(C_STRING()));
            }
            Err(e) => {
                ui.label(RichText::new(e).color(ALERT()));
            }
        }
        let mut keep = true;
        let mut create = false;
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
            create = ui.add_enabled(spec.is_ok(), primary_button(ui, "Create", PRIMARY())).clicked();
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                keep = false;
            }
        });
        // The dialog closes once the create is sent; a refusal already said why and keeps it open.
        if let (true, Ok(spec), Some(tab)) = (create, spec, self.t().map(|t| t.uid)) && self.core.execute(Action::Indexes(IdxA::Create { tab, spec })).is_ok() {
            keep = false;
        }
        keep
    }

    pub(super) fn index_act(&mut self, ctx: &egui::Context, row: &Document, act: IdxAct) {
        let name = row.get_str("name").unwrap_or_default().to_string();
        let keys = row.get("keys").cloned().unwrap_or_else(|| Bson::Document(doc! {}));
        match act {
            IdxAct::CopyKeys => {
                ctx.copy_text(db::shell_text(&keys));
                self.ok("Copied the keys");
            }
            IdxAct::CopyCmd => {
                let Some(t) = self.t() else { return };
                let coll = collection_ref(&t.sel.coll);
                let coll = if coll.starts_with("db.") { coll } else { format!("db.{coll}") };
                let mut opts = row.get_document("options").cloned().unwrap_or_default();
                // A clustered collection's _id index is made with the collection, never by createIndex.
                opts.remove("clustered");
                opts.insert("name", &name);
                ctx.copy_text(format!("{coll}.createIndex({}, {})", db::shell_text(&keys), db::shell_text(&Bson::Document(opts))));
                self.ok("Copied the createIndex command");
            }
            IdxAct::Hide(hidden) => {
                if let Some(tab) = self.t().map(|t| t.uid) {
                    self.exec(Action::Indexes(IdxA::Hide { tab, name, hidden }));
                }
            }
            IdxAct::Drop => {
                self.modal = Some(Modal::Typed {
                    title: format!("Drop the index {name}? Queries that rely on it may become slow."),
                    expect: name.clone(),
                    typed: String::new(),
                    action: self.pinned(Danger::DropIndex(name)),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_form_builds_the_spec() {
        let f = IndexForm { keys: "{ email: 1, at: -1 }".into(), unique: true, partial: "{ active: true }".into(), locale: "fr".into(), strength: 2, ..Default::default() };
        assert_eq!(f.spec().unwrap(), doc! { "key": { "email": 1, "at": -1 }, "name": "email_1_at_-1", "unique": true, "partialFilterExpression": { "active": true }, "collation": { "locale": "fr", "strength": 2 } });
        let ttl = IndexForm { keys: "{ at: 1 }".into(), ttl: "3600".into(), ..Default::default() };
        assert_eq!(ttl.spec().unwrap(), doc! { "key": { "at": 1 }, "name": "at_1", "expireAfterSeconds": 3600_i64 });
        assert_eq!(IndexForm { keys: "{ body: 'text' }".into(), name: " search ".into(), ..Default::default() }.spec().unwrap(), doc! { "key": { "body": "text" }, "name": "search" });
        assert!(IndexForm { keys: "{}".into(), ..Default::default() }.spec().is_err());
        assert!(IndexForm { keys: "{ a: 1 }".into(), ttl: "soon".into(), ..Default::default() }.spec().is_err());
        assert!(IndexForm { keys: "{ a: 1 }".into(), ttl: "-5".into(), ..Default::default() }.spec().is_err());
        assert!(IndexForm { keys: "{ a: 1, b: 1 }".into(), ttl: "60".into(), ..Default::default() }.spec().is_err());
        assert!(IndexForm { keys: "{ a: 1 }".into(), sparse: true, partial: "{ a: 1 }".into(), ..Default::default() }.spec().is_err());
    }
}
