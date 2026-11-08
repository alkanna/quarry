//! The indexes view: one row per index in the shared results table, profiler suggestions, and
//! the dialog that creates an index with all its options.

use super::*;

/// Columns of the Indexes table.
pub(super) static IDX_FIELDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| ["name", "keys", "options", "status", "ops", "since", "size"].map(String::from).to_vec());

/// A week, in ms: a usage counter younger than this cannot prove an index unused.
const WEEK_MS: i64 = 7 * 24 * 3600 * 1000;

/// One table row per index: its keys and options with the usage stats merged in.
pub(super) fn index_rows(indexes: &[Document], info: Option<&db::IndexInfo>, now: i64) -> Vec<Document> {
    indexes
        .iter()
        .map(|ix| {
            let name = ix.get_str("name").unwrap_or_default().to_string();
            let mut opts = ix.clone();
            opts.remove("name");
            let keys = opts.remove("keys").unwrap_or_else(|| Bson::Document(doc! {}));
            let stat = info.and_then(|i| i.stats.iter().find(|s| s.name == name));
            let mut status = Vec::new();
            if stat.is_some_and(|s| s.hidden) || opts.get_bool("hidden").unwrap_or(false) {
                status.push("hidden");
            }
            let mut row = doc! { "name": &name, "keys": keys };
            if !opts.is_empty() {
                row.insert("options", opts);
            }
            if let Some(s) = stat {
                if name != "_id_" && db::is_unused(s.ops, s.since, now) {
                    status.push("unused");
                } else if s.ops == 0 && s.since.is_some_and(|t| now - t < WEEK_MS) {
                    status.push("no use since restart");
                }
                row.insert("ops", s.ops);
                if let Some(t) = s.since {
                    row.insert("since", mongodb::bson::DateTime::from_millis(t));
                }
                if let Some(b) = s.size {
                    row.insert("size", human_bytes(b));
                }
            }
            if !status.is_empty() {
                row.insert("status", status.join(", "));
            }
            row
        })
        .collect()
}

/// Why the usage columns are empty, without the raw command the server echoes back.
fn stats_note(e: &str) -> &'static str {
    if e.contains("not authorized") || e.contains("is not allowed to do action") {
        "Usage counts are not available: this user may not read index statistics here (it needs the indexStats privilege, part of clusterMonitor)."
    } else {
        "Usage counts are not available for this collection."
    }
}

/// The name the server would give an index with these keys, like `email_1_createdAt_-1`.
fn default_index_name(keys: &Document) -> String {
    let part = |v: &Bson| match v {
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) => f.to_string(),
        other => db::compact(other),
    };
    keys.iter().map(|(k, v)| format!("{k}_{}", part(v))).collect::<Vec<_>>().join("_")
}

/// What the New index dialog holds.
#[derive(Default)]
pub(super) struct IndexForm {
    pub(super) keys: String,
    name: String,
    unique: bool,
    sparse: bool,
    hidden: bool,
    ttl: String,
    partial: String,
    locale: String,
    strength: i32,
}

impl IndexForm {
    /// The index entry of a createIndexes command, or what is wrong with the form.
    fn spec(&self) -> Result<Document, String> {
        let keys = db::parse_doc(&self.keys).map_err(|e| format!("Keys: {e}"))?;
        if keys.is_empty() {
            return Err("Give at least one key, like { email: 1 }.".into());
        }
        let name = if self.name.trim().is_empty() { default_index_name(&keys) } else { self.name.trim().to_string() };
        let mut s = doc! { "key": keys, "name": name };
        for (on, k) in [(self.unique, "unique"), (self.sparse, "sparse"), (self.hidden, "hidden")] {
            if on {
                s.insert(k, true);
            }
        }
        if !self.ttl.trim().is_empty() {
            let n: i64 = self.ttl.trim().parse().map_err(|_| "Expire after: give a whole number of seconds.")?;
            s.insert("expireAfterSeconds", n);
        }
        if !self.partial.trim().is_empty() {
            s.insert("partialFilterExpression", db::parse_doc(&self.partial).map_err(|e| format!("Partial filter: {e}"))?);
        }
        if !self.locale.trim().is_empty() {
            s.insert("collation", doc! { "locale": self.locale.trim(), "strength": self.strength.clamp(1, 5) });
        }
        Ok(s)
    }
}

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
    let hidden = row.get_str("status").is_ok_and(|s| s.contains("hidden"));
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
        let key = format!("{}.{}", t.sel.db, t.sel.coll);
        let Some(i) = self.idx_info.as_ref().filter(|(k, _)| *k == key).map(|(_, i)| i) else { return };
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
            if let Some((conn, target)) = self.writable_target() {
                self.worker.db_change(conn, target.client, target.db, doc! { "profile": 1, "slowms": 100 }, "Profiling slow operations".into());
            }
        }
    }

    pub(super) fn open_create_index(&mut self, keys: Option<Document>) {
        let keys = keys.map_or_else(|| "{  }".into(), |k| db::shell_text(&Bson::Document(k)));
        self.modal = Some(Modal::CreateIndex(Box::new(IndexForm { keys, strength: 3, ..Default::default() })));
    }

    /// The New index dialog; returns whether it stays open.
    pub(super) fn create_index_dialog(&mut self, ui: &mut egui::Ui, f: &mut IndexForm) -> bool {
        let Some(t) = self.t() else { return false };
        let coll = t.sel.coll.clone();
        let (paths, fields) = (t.paths.clone(), t.field_info.clone());
        dialog_title(ui, format!("New index on {coll}"));
        caption(ui, "KEYS");
        let id = egui::Id::unique("index-keys");
        let mut ac = std::mem::take(&mut self.ac);
        ac_field(ui, id, &mut f.keys, &mut ac, &|b| suggest(b, FieldKind::Sort, &paths, &fields), |ui, text, _| {
            egui::TextEdit::singleline(text).id(id).font(egui::TextStyle::Monospace).hint_text("{ email: 1, createdAt: -1 }").desired_width(440.0).show(ui)
        });
        self.ac = ac;
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
        let cmd = spec.as_ref().ok().map(|s| doc! { "createIndexes": &coll, "indexes": [s.clone()] });
        match (&cmd, &spec) {
            (Some(c), _) => {
                caption(ui, "COMMAND");
                ui.label(RichText::new(db::shell_text(&Bson::Document(c.clone()))).monospace().color(C_STRING()));
            }
            (None, Err(e)) => {
                ui.label(RichText::new(e).color(ALERT()));
            }
            _ => {}
        }
        let mut keep = true;
        let mut create = false;
        dialog_actions(ui, |ui| {
            create = ui.add_enabled(cmd.is_some(), primary_button(ui, "Create", PRIMARY())).clicked();
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                keep = false;
            }
        });
        if let (true, Some(cmd), Ok(s)) = (create, cmd, spec) {
            if let Some((conn, target)) = self.writable_target() {
                let done = format!("Created index {}", s.get_str("name").unwrap_or_default());
                self.worker.db_change(conn, target.client, target.db, cmd, done);
                keep = false;
            }
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
                opts.insert("name", &name);
                ctx.copy_text(format!("{coll}.createIndex({}, {})", db::shell_text(&keys), db::shell_text(&Bson::Document(opts))));
                self.ok("Copied the createIndex command");
            }
            IdxAct::Hide(hidden) => {
                if let Some((conn, target)) = self.writable_target() {
                    let done = format!("{} index {name}", if hidden { "Hid" } else { "Unhid" });
                    self.worker.db_change(conn, target.client, target.db, doc! { "collMod": target.coll, "index": { "name": name, "hidden": hidden } }, done);
                }
            }
            IdxAct::Drop => {
                self.modal = Some(Modal::Typed {
                    title: format!("Drop the index {name}? Queries that rely on it may become slow."),
                    expect: name.clone(),
                    typed: String::new(),
                    action: Danger::DropIndex(name),
                });
            }
        }
    }

    /// Rebuilds every tab's index rows from its indexes and the latest usage stats.
    pub(super) fn refresh_idx_rows(&mut self) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        for t in &mut self.tabs {
            let key = format!("{}.{}", t.sel.db, t.sel.coll);
            let info = self.idx_info.as_ref().filter(|(k, _)| *k == key).map(|(_, i)| i);
            t.idx_rows = index_rows(&t.indexes, info, now);
        }
    }

    pub(super) fn load_indexes(&mut self) {
        self.load_indexes_of(self.cur);
    }

    pub(super) fn load_indexes_of(&mut self, i: usize) {
        if let Some(target) = self.tabs.get(i).and_then(|t| self.target_for(&t.sel)) {
            self.req += 1;
            let req = self.req;
            self.tabs[i].idx_req = req;
            // Only the Indexes view shows usage, and it costs a profiler query.
            if self.tabs[i].view == View::Indexes {
                self.worker.index_info(target.clone());
            }
            self.worker.indexes(req, target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_rows_merge_usage() {
        let ixs = [doc! { "name": "_id_", "keys": { "_id": 1 } }, doc! { "name": "email_1", "keys": { "email": 1 }, "unique": true }];
        let day = 24 * 3600 * 1000;
        let now = 100 * day;
        let stat = |name: &str, ops, since| db::IndexStat { name: name.into(), ops, since: Some(since), hidden: false, size: Some(2048) };
        let info = db::IndexInfo { stats: vec![stat("_id_", 0, 0), stat("email_1", 0, 0)], stats_err: None, level: 0, slowms: 100, shapes: Vec::new() };
        let rows = index_rows(&ixs, Some(&info), now);
        assert_eq!(rows[0].get_str("status"), Err(mongodb::bson::document::ValueAccessError::NotPresent), "_id_ is never unused");
        assert_eq!(rows[1].get_str("status").unwrap(), "unused");
        assert_eq!(rows[1].get_document("options").unwrap(), &doc! { "unique": true });
        assert_eq!(rows[1].get_str("size").unwrap(), "2 KB");
        let info = db::IndexInfo { stats: vec![stat("email_1", 0, now - day)], ..info };
        assert_eq!(index_rows(&ixs, Some(&info), now)[1].get_str("status").unwrap(), "no use since restart");
        assert!(index_rows(&ixs, None, now)[1].get("ops").is_none());
    }

    #[test]
    fn refused_index_stats_get_a_short_note() {
        let atlas = "Command failed: Error code 13 (Unauthorized): not authorized on admin to execute command { aggregate: \"system.roles\", pipeline: [ { $indexStats: {} } ] }";
        assert!(stats_note(atlas).contains("indexStats privilege"));
        assert_eq!(stats_note("Command failed: something else"), "Usage counts are not available for this collection.");
    }

    #[test]
    fn index_form_builds_the_spec() {
        let f = IndexForm { keys: "{ email: 1, at: -1 }".into(), unique: true, ttl: "3600".into(), partial: "{ active: true }".into(), locale: "fr".into(), strength: 2, ..Default::default() };
        assert_eq!(
            f.spec().unwrap(),
            doc! { "key": { "email": 1, "at": -1 }, "name": "email_1_at_-1", "unique": true, "expireAfterSeconds": 3600_i64, "partialFilterExpression": { "active": true }, "collation": { "locale": "fr", "strength": 2 } }
        );
        assert_eq!(IndexForm { keys: "{ body: 'text' }".into(), name: " search ".into(), ..Default::default() }.spec().unwrap(), doc! { "key": { "body": "text" }, "name": "search" });
        assert!(IndexForm { keys: "{}".into(), ..Default::default() }.spec().is_err());
        assert!(IndexForm { keys: "{ a: 1 }".into(), ttl: "soon".into(), ..Default::default() }.spec().is_err());
    }
}
