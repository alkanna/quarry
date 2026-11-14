//! The New database and New collection dialog of a MongoDB connection, with the options of `create`.
//! MongoDB makes a database with its first collection, so one form serves both.

use super::*;
use crate::db::collopts::{self, CollForm, Granularity, Kind};

pub(super) struct MongoNewColl {
    pub(super) conn: u64,
    pub(super) db: String,
    /// Asks for the database name too; the database does not exist yet.
    pub(super) new_db: bool,
    pub(super) name: String,
    pub(super) form: CollForm,
}

impl App {
    pub(super) fn mongo_new_db(&mut self, conn: u64) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.mongo_new_db(conn));
        }
        self.modal = Some(Modal::MongoNewColl(Box::new(MongoNewColl { conn, db: String::new(), new_db: true, name: String::new(), form: CollForm::default() })));
    }

    pub(super) fn mongo_new_coll(&mut self, conn: u64, db: String) {
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.mongo_new_coll(conn, db));
        }
        self.modal = Some(Modal::MongoNewColl(Box::new(MongoNewColl { conn, db, new_db: false, name: String::new(), form: CollForm::default() })));
    }

    /// The first thing wrong with the form, which keeps Create disabled and is said in the footer.
    fn mongo_new_coll_problem(&self, f: &MongoNewColl, ver: collopts::Ver) -> Option<String> {
        let (db, name) = (f.db.trim(), f.name.trim());
        if f.new_db {
            if let Some(p) = collopts::db_name_problem(db) {
                return Some(p);
            }
            // Mongo names are case sensitive but two that differ only by case collide on disk, and the server refuses it.
            if self.conn(f.conn).is_some_and(|c| c.dbs.iter().any(|d| d.0.eq_ignore_ascii_case(db))) {
                return Some(format!("A database named {db} (or one that differs only by case) already exists."));
            }
        }
        if let Some(p) = collopts::coll_name_problem(name, db) {
            return Some(p);
        }
        // Since MongoDB 7 create succeeds on a collection that exists with the same options, so the list is checked here.
        if !f.new_db && self.conn(f.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == f.db)).and_then(|d| d.1.as_deref()).is_some_and(|l| l.iter().any(|c| c == name)) {
            return Some(format!("{name} already exists in {db}."));
        }
        collopts::create_command(name, &f.form, ver).err()
    }

    pub(super) fn mongo_new_coll_dialog(&mut self, ui: &mut egui::Ui, f: &mut MongoNewColl, keep: &mut bool) {
        ui.set_width(540.0);
        let cname = self.conn_name(f.conn);
        let ver = self.conn(f.conn).and_then(|c| collopts::parse_ver(&c.server.version));
        let head = if f.new_db { Head::new("New database").sub(format!("on {cname}  ·  made with its first collection")) } else { Head::new("New collection").sub(format!("in {} on {cname}", f.db)) };
        dialog_head(ui, &head.glyph(glyphs::plus));
        dialog_body(ui, |ui| {
            if f.new_db {
                form_card(ui, "", "", |rows| {
                    rows.label_width(120.0);
                    rows.row("Database", "", |ui| ui.add(egui::TextEdit::singleline(&mut f.db).desired_width(ui.available_width())));
                    rows.row("First collection", "", |ui| ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(ui.available_width())));
                });
            } else {
                stacked_field(ui, "Name", "", |ui| ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(f32::INFINITY)));
            }
            let summary = if f.form.is_plain() { "Default settings".to_string() } else { options_summary(&f.form) };
            egui::CollapsingHeader::new(medium(RichText::new("Options").size(TXT_SMALL).color(TEXT()))).id_salt("mongo-coll-options").default_open(false).show(ui, |ui| {
                ui.add_space(4.0);
                options_form(ui, &mut f.form, ver);
            });
            ui.label(RichText::new(summary).size(TXT_SMALL).color(MUTED()));
        });
        let problem = self.mongo_new_coll_problem(f, ver);
        dialog_foot(ui, |ui| {
            if ui.add_enabled(problem.is_none(), primary_button(ui, "Create", PRIMARY())).clicked() {
                self.mongo_new_coll_run(f, ver, keep);
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
            // A bare name that is merely empty is not worth a message on a form just opened.
            if let Some(p) = problem.as_ref().filter(|_| !(f.name.trim().is_empty() && (!f.new_db || f.db.trim().is_empty()))) {
                foot_left(ui, |ui| foot_note(ui, p));
            }
        });
    }

    fn mongo_new_coll_run(&mut self, f: &MongoNewColl, ver: collopts::Ver, keep: &mut bool) {
        *keep = false;
        self.mongo_create_send(f.conn, f.db.trim().to_string(), f.name.trim().to_string(), f.form.clone(), ver);
    }

    /// Runs the create. Writes can be locked again while the dialog is open: the unlock dialog then runs the same create once it is accepted, so the typed form is not lost.
    fn mongo_create_send(&mut self, conn: u64, db: String, name: String, form: CollForm, ver: collopts::Ver) {
        let cmd = match collopts::create_command(&name, &form, ver) {
            Ok(c) => c,
            Err(e) => return self.err(e),
        };
        if !self.conn_writable(conn) {
            return self.ask_unlock_on(conn, move |a| a.mongo_create_send(conn, db, name, form, ver));
        }
        let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else {
            return self.warn_offline();
        };
        self.open_after = Some((conn, db.clone(), name.clone()));
        self.worker.db_change(conn, client, db.clone(), cmd, format!("Created {db}.{name}"));
    }
}

fn options_summary(f: &CollForm) -> String {
    let mut parts = vec![f.kind.label().to_lowercase()];
    if f.collate && f.kind.takes_rules() {
        parts.push(format!("collation {}", f.locale.trim()));
    }
    if f.validate && f.kind.takes_rules() {
        parts.push("validator".into());
    }
    parts.join(", ")
}

fn options_form(ui: &mut egui::Ui, f: &mut CollForm, ver: collopts::Ver) {
    card_caption(ui, "Type", "");
    let kinds: Vec<Kind> = Kind::ALL.into_iter().filter(|k| k.supported(ver)).collect();
    let w = (ui.available_width() - 8.0) / 2.0;
    for pair in kinds.chunks(2) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for k in pair {
                if radio_card(ui, w, f.kind == *k, k.label(), k.blurb()).clicked() {
                    f.kind = *k;
                }
            }
        });
        ui.add_space(8.0);
    }
    match f.kind {
        Kind::Plain => {}
        Kind::Capped => form_card(ui, "Capped", "", |rows| {
            rows.label_width(170.0);
            rows.row("Size in bytes", "required", |ui| ui.add(egui::TextEdit::singleline(&mut f.capped_size).desired_width(ui.available_width())));
            rows.row("Maximum documents", "optional", |ui| ui.add(egui::TextEdit::singleline(&mut f.capped_max).desired_width(ui.available_width())));
        }),
        Kind::TimeSeries => form_card(ui, "Time series", "", |rows| {
            rows.label_width(170.0);
            rows.row("Time field", "required", |ui| ui.add(egui::TextEdit::singleline(&mut f.ts_time).desired_width(ui.available_width())));
            rows.row("Meta field", "optional", |ui| ui.add(egui::TextEdit::singleline(&mut f.ts_meta).desired_width(ui.available_width())));
            // Bucket spans replace the granularity from 6.3.
            let spans = collopts::at_least(ver, 6, 3);
            if spans {
                rows.row("Custom bucket spans", "", |ui| toggle(ui, &mut f.ts_custom));
            }
            if spans && f.ts_custom {
                rows.row("Bucket span", "seconds", |ui| ui.add(egui::TextEdit::singleline(&mut f.ts_span).desired_width(ui.available_width())));
                rows.row("Bucket rounding", "seconds, equal to the span", |ui| ui.add(egui::TextEdit::singleline(&mut f.ts_round).desired_width(ui.available_width())));
            } else {
                rows.row("Granularity", "", |ui| {
                    let all: Vec<(Granularity, &str, bool)> = Granularity::ALL.iter().map(|g| (*g, g.word(), true)).collect();
                    segmented_on(ui, &mut f.ts_gran, &all);
                });
            }
            rows.row("Expire after", "seconds, optional", |ui| ui.add(egui::TextEdit::singleline(&mut f.ts_expire).desired_width(ui.available_width())));
        }),
        Kind::Clustered => form_card(ui, "Clustered", "", |rows| {
            rows.label_width(170.0);
            rows.row("Clustered on", "always unique", |ui| ui.label(RichText::new("_id").family(egui::FontFamily::Monospace).color(TEXT())));
            rows.row("Expire after", "seconds, optional", |ui| ui.add(egui::TextEdit::singleline(&mut f.cl_expire).desired_width(ui.available_width())));
        }),
    }
    // A time series collection takes neither, so the choice is not offered rather than refused by the server.
    if !f.kind.takes_rules() {
        return;
    }
    form_card(ui, "Collation", "", |rows| {
        rows.label_width(170.0);
        rows.row("Use a collation", "how strings compare and sort", |ui| toggle(ui, &mut f.collate));
        if f.collate {
            rows.row("Locale", "such as en, fr, de", |ui| ui.add(egui::TextEdit::singleline(&mut f.locale).desired_width(ui.available_width())));
            rows.row("Strength", "1 ignores case and accents, 3 is the default", |ui| {
                let opts: Vec<(u8, &str, bool)> = (1..=5u8).map(|n| (n, ["1", "2", "3", "4", "5"][n as usize - 1], true)).collect();
                segmented_on(ui, &mut f.strength, &opts);
            });
        }
    });
    form_card(ui, "Validator", "", |rows| {
        rows.label_width(170.0);
        rows.row("Validate documents", "a JSON schema or a query", |ui| toggle(ui, &mut f.validate));
        if f.validate {
            rows.stacked("Validator", |ui| {
                let id = ui.make_persistent_id("mongo-coll-validator");
                crate::ui::vim::wrap(ui, id, &mut f.validator, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(6).layouter(&mut code_layout).show(ui)));
            });
            rows.row("Level", "which writes are checked", |ui| {
                let opts = [("strict".to_string(), "strict", true), ("moderate".to_string(), "moderate", true), ("off".to_string(), "off", true)];
                segmented_str(ui, &mut f.level, &opts);
            });
            rows.row("On failure", "", |ui| {
                let opts = [("error".to_string(), "error", true), ("warn".to_string(), "warn", true)];
                segmented_str(ui, &mut f.action, &opts);
            });
        }
    });
}

/// `segmented_on` over a String, which is not Copy.
fn segmented_str(ui: &mut egui::Ui, value: &mut String, options: &[(String, &str, bool)]) {
    let mut at = options.iter().position(|o| o.0 == *value).unwrap_or(0);
    let idx: Vec<(usize, &str, bool)> = options.iter().enumerate().map(|(i, o)| (i, o.1, o.2)).collect();
    segmented_on(ui, &mut at, &idx);
    *value = options[at].0.clone();
}
