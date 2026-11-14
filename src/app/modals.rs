//! The dialog layer: the JSON editor, insert, and the confirmations and pickers in `Modal`.
//! Larger dialogs live with their feature and are called from here.

use super::*;

impl App {
    pub(super) fn modals(&mut self, ctx: &egui::Context) {
        self.value_dialog(ctx);
        let into = self.t().map(|t| format!("into {}", t.sel.ns())).unwrap_or_default();
        if let Some(text) = &mut self.insert {
            let mut close = false;
            let mut submit = false;
            let r = banded(egui::Id::unique(("quarry-layer", "insert"))).show(ctx, |ui| {
                ui.set_width(600.0);
                dialog_head(ui, &Head::new("Insert a document").sub(into).glyph(glyphs::plus));
                dialog_pane(ui, |ui| {
                    ui.label(RichText::new("JSON, JSON5 or shell syntax: ObjectId(\"…\"), ISODate(\"…\"), NumberLong(…).").size(TXT_SMALL).color(MUTED()));
                    ui.add_space(6.0);
                    let id = egui::Id::unique("insert-editor-text");
                    let find = Find::bar(ui, id, text);
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        let out = crate::ui::vim::wrap(ui, id, text, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(16).layouter(&mut find.layouter(true)).show(ui)));
                        find.reveal(ui, text, &out);
                    });
                });
                dialog_foot(ui, |ui| {
                    submit = primary(ui, "Insert", PRIMARY()).clicked();
                    close = ui.button("Cancel").on_hover_text("Esc").clicked();
                });
            });
            if submit {
                match db::parse_doc(text) {
                    Ok(d) => {
                        if let Some((conn, target)) = self.writable_target() {
                            self.worker.insert(conn, target, d, self.insert.take());
                        }
                    }
                    Err(e) => self.err(e),
                }
            } else if close || r.should_close() {
                self.insert = None;
            }
        }

        // Settings looks for gcloud each time it opens, so it forgets the answer once closed.
        if self.cloud.gcloud_asked && !matches!(self.modal, Some(Modal::Settings)) {
            self.gcloud_forget();
        }
        if matches!(self.modal, Some(Modal::Palette { .. })) {
            return self.palette(ctx);
        }
        if matches!(self.modal, Some(Modal::Settings)) {
            return self.settings(ctx);
        }
        let Some(mut m) = self.modal.take() else { return };
        let mut keep = true;
        let id = egui::Id::unique(("quarry-layer", "modal"));
        let r = banded(id).show(ctx, |ui| {
            ui.set_max_width(960.0);
            match &mut m {
                Modal::Conn(f) => self.conn_dialog(ui, f, &mut keep),
                Modal::Attach(f) => self.attach_dialog(ui, f, &mut keep),
                Modal::GroupEdit(f) => self.group_dialog(ui, f, &mut keep),
                Modal::SaveScript(f) => self.save_dialog(ui, f, &mut keep),
                Modal::OpenOn(f) => self.open_on_dialog(ui, f, &mut keep),
                Modal::LinkFolder(f) => self.link_dialog(ui, f, &mut keep),
                Modal::Unlock(id, then) => {
                    let name = self.conn(*id).map(|c| c.meta.name.clone()).unwrap_or_default();
                    ui.set_width(460.0);
                    dialog_head(ui, &Head::new(format!("Allow writes on \"{name}\"?")).sub("Every connection starts read-only").glyph(glyphs::lock).tone(WARNING()));
                    dialog_body(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new("Edits, inserts, deletes and index changes become possible until you lock it again or quit. Each one still asks first.").color(TEXT())).wrap());
                    });
                    dialog_foot(ui, |ui| {
                        if primary(ui, "Allow writes", ALERT()).clicked() {
                            if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == *id) {
                                c.writable = true;
                            }
                            self.ok(format!("Writes allowed on {name} for this session"));
                            keep = false;
                            if let Some(then) = then.take() {
                                then(self);
                            }
                        }
                        if ui.button("Stay read-only").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Save { tag, changes } => {
                    ui.set_width(620.0);
                    dialog_head(ui, &Head::new(format!("Save {} change(s)?", changes.len())).sub("Written as one update").glyph(glyphs::check));
                    dialog_body(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new("Saving is refused if someone else changed this document since you opened it.").color(TEXT())).wrap());
                        ui.add_space(10.0);
                        diff_table(ui, "chg", changes.iter().cloned());
                    });
                    dialog_foot(ui, |ui| {
                        if primary(ui, "Save", PRIMARY()).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            self.write_draft(tag);
                            keep = false;
                        }
                        if ui.button("Keep editing").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Delete(at, docs) => self.delete_dialog(ui, at, docs, &mut keep),
                Modal::Export(f) => self.export_dialog(ui, f, &mut keep),
                Modal::SqlImport(f) => self.sql_import_dialog(ui, f, &mut keep),
                Modal::SqlDump(f) => self.sql_dump_dialog(ui, f, &mut keep),
                Modal::SqlRunFile(f) => self.sql_run_file_dialog(ui, f, &mut keep),
                Modal::SqlSet(f) => self.sql_set_dialog(ui, f, &mut keep),
                Modal::SqlRow(f) => self.sql_row_dialog(ui, f, &mut keep),
                Modal::ColEdit(f) => self.col_dialog(ui, f, &mut keep),
                Modal::IdxEdit(f) => self.idx_dialog(ui, f, &mut keep),
                Modal::CheckEdit(f) => self.check_dialog(ui, f, &mut keep),
                Modal::SqlNewDb(f) => self.sql_new_db_dialog(ui, f, &mut keep),
                Modal::SqlNewTable(f) => self.sql_new_table_dialog(ui, f, &mut keep),
                Modal::SqlDrop(f) => self.sql_drop_dialog(ui, f, &mut keep),
                Modal::PgNewDb(f) => self.pg_new_db_dialog(ui, f, &mut keep),
                Modal::PgNewSchema(f) => self.pg_new_schema_dialog(ui, f, &mut keep),
                Modal::PgNewTable(f) => self.pg_new_table_dialog(ui, f, &mut keep),
                Modal::SqlNewUser(f) => self.sql_new_user_dialog(ui, f, &mut keep),
                Modal::SqlPassword(f) => self.sql_password_dialog(ui, f, &mut keep),
                Modal::PgNewRole(f) => self.pg_new_role_dialog(ui, f, &mut keep),
                Modal::PgPassword(f) => self.pg_password_dialog(ui, f, &mut keep),
                Modal::PgDropRole(f) => self.pg_drop_role_dialog(ui, f, &mut keep),
                Modal::FkEdit(f) => self.fk_dialog(ui, f, &mut keep),
                Modal::Typed { title, expect, typed, action } => {
                    ui.set_width(480.0);
                    dialog_head(ui, &Head::new("Type to confirm").glyph(glyphs::warn).tone(ALERT()));
                    dialog_body(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(title.as_str()).color(TEXT())).wrap());
                        ui.add_space(12.0);
                        typed_confirm(ui, expect, typed);
                    });
                    dialog_foot(ui, |ui| {
                        let ok = typed == expect;
                        if danger_solid(ui, ok, "Confirm").clicked() {
                            keep = false;
                            let action = std::mem::replace(action, Danger::DropCollection);
                            self.danger(action);
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::NewCollection { conn, db, name, view, source, pipeline } => self.new_collection_dialog(ui, conn, db, name, view, source, pipeline, &mut keep),
                Modal::Tool(f) => self.tool_dialog(ui, f, &mut keep),
                Modal::Relations { conn, db } => self.relations_dialog(ui, conn, db, &mut keep),
                Modal::RenameColl { conn, db, coll, name } => {
                    ui.set_width(460.0);
                    let cname = self.conn_name(*conn);
                    dialog_head(ui, &Head::new(format!("Rename {coll}")).sub(format!("in {db} on {cname}")).glyph(glyphs::doc));
                    dialog_body(ui, |ui| {
                        stacked_field(ui, "New name", "Open tabs of this collection are closed, as in the original.", |ui| ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)));
                    });
                    dialog_foot(ui, |ui| {
                        let ok = !name.trim().is_empty() && name.trim() != coll.as_str();
                        if ui.add_enabled(ok, primary_button(ui, "Rename", PRIMARY())).clicked() {
                            if let Some(client) = self.conn(*conn).and_then(|c| c.client.clone()) {
                                let (from, to) = (format!("{db}.{coll}"), format!("{db}.{}", name.trim()));
                                let (c, d, k) = (*conn, db.clone(), coll.clone());
                                self.remove_tabs(|t| t.sel.conn == c && t.sel.db == d && t.sel.coll == k);
                                self.worker.db_change(*conn, client, db.clone(), doc! { "renameCollection": from, "to": to }, format!("Renamed {coll} to {}", name.trim()));
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::DuplicateColl { conn, db, coll, name } => self.duplicate_coll_dialog(ui, conn, db, coll, name, &mut keep),
                Modal::ImportInsert(n) => {
                    let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                    ui.set_width(460.0);
                    dialog_head(ui, &Head::new(format!("Insert {n} documents into {coll}?")).sub("Import").glyph(glyphs::upload));
                    dialog_body(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new("They are added as new documents. Undo removes exactly these documents again.").color(TEXT())).wrap());
                    });
                    dialog_foot(ui, |ui| {
                        if primary(ui, "Import", PRIMARY()).clicked() {
                            if let Some((conn, target)) = self.writable_target() && let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                                self.worker.import(conn, target, docs, None);
                                self.ok("Importing…");
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ShellWrite(ops) => {
                    ui.set_width(480.0);
                    dialog_head(ui, &Head::new("Execute this command?").sub("Shell").glyph(glyphs::warn).tone(ALERT()));
                    dialog_body(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new("Your command contains potentially destructive operations:").color(TEXT())).wrap());
                        ui.add_space(10.0);
                        code_card(ui, "", |ui| ui.add(egui::Label::new(RichText::new(ops.join(", ")).monospace().color(ALERT())).wrap()));
                    });
                    dialog_foot(ui, |ui| {
                        if danger_solid(ui, true, "Execute").clicked() {
                            self.start_shell();
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::AggWrite(p) => {
                    ui.set_width(480.0);
                    dialog_head(ui, &Head::new("Run a pipeline that writes?").sub("It ends with $out or $merge").glyph(glyphs::warn).tone(ALERT()));
                    dialog_body(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new("It can replace or change a whole collection and cannot be undone here.").color(TEXT())).wrap());
                    });
                    dialog_foot(ui, |ui| {
                        if danger_solid(ui, true, "Run it").clicked() {
                            if let Some((_, target)) = self.writable_target() {
                                self.req += 1;
                                let req = self.req;
                                self.tm().unwrap().agg_req = req;
                                self.worker.aggregate(req, target, p.clone());
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::SaveAll(keys) => self.save_all_dialog(ui, keys, &mut keep),
                Modal::Grouped { sel, path, pipeline, rows } => self.grouped_dialog(ui, sel, path, pipeline, rows, &mut keep),
                Modal::ColumnCustomize { key, agg, col, label, path } => {
                    ui.set_width(460.0);
                    dialog_head(ui, &Head::new(format!("Customize column {col}")).sub("Its header and the field it shows").glyph(glyphs::sliders));
                    let p = path.trim().to_string();
                    let valid = !p.starts_with('$') && !p.split('.').any(str::is_empty);
                    dialog_body(ui, |ui| {
                        stacked_field(ui, "Column name", "", |ui| ui.add(egui::TextEdit::singleline(label).hint_text(col.as_str()).desired_width(f32::INFINITY)));
                        stacked_field(ui, "Field path", "A dotted path, like customer.name", |ui| ui.add(egui::TextEdit::singleline(path).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)));
                    });
                    dialog_foot(ui, |ui| {
                        if ui.add_enabled(valid, primary_button(ui, "Save", PRIMARY())).clicked() {
                            let fields = self.t().map(|t| if *agg { t.agg_source().1.to_vec() } else { t.fields.clone() }).unwrap_or_default();
                            self.apply_col_acts(key, &fields, vec![ColAct::Customize { col: col.clone(), label: label.clone(), path: path.trim().to_string() }]);
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ErrorDetails(e) => keep = self.error_details(ui, e),
                Modal::Text { title, body, code } => {
                    // Wide enough for the longest line, so column layouts like the Query help do not wrap.
                    let longest = body.lines().map(|l| l.chars().count()).max().unwrap_or(0);
                    let text_w = ui.painter().layout_no_wrap("0".repeat(longest), egui::FontId::monospace(TXT_BODY), TEXT()).size().x;
                    ui.set_width((text_w + 80.0).clamp(560.0, 820.0));
                    let id = egui::Id::unique("text-modal");
                    let mut tree = false;
                    // JSON can also be read as a folding tree; the switch sits in the header beside the close button.
                    dialog_head_with(ui, &Head::new(title.as_str()).glyph(if *code { glyphs::code } else { glyphs::doc }), |ui| {
                        if crate::ui::json_tree::looks_like(body) {
                            tree = crate::ui::json_tree::mode_switch(ui, id);
                        }
                    });
                    dialog_pane(ui, |ui| {
                        if tree {
                            crate::ui::json_tree::scrolled(ui, id, body, 0.0, 420.0);
                        } else {
                            let find = Find::bar(ui, id, body);
                            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                                let mut text = body.as_str();
                                let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(*code)).show(ui);
                                find.reveal(ui, body, &out);
                            });
                        }
                    });
                    dialog_foot(ui, |ui| {
                        if ui.button("Close").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                        foot_left(ui, |ui| {
                            if ui.button("Copy").clicked() {
                                ui.ctx().copy_text(body.clone());
                            }
                        });
                    });
                }
                Modal::CreateIndex(f) => keep = self.create_index_dialog(ui, f),
                Modal::Rename { doc: d, path, name } => self.rename_field_dialog(ui, d, path, name, &mut keep),
                Modal::UpdateMany { set_txt, unset_txt, preview } => self.update_many_dialog(ui, set_txt, unset_txt, preview, &mut keep),
                Modal::Undo => self.undo_dialog(ui, &mut keep),
                Modal::Explain { summary, raw, show_raw } => self.explain_dialog(ui, summary, raw, show_raw, &mut keep),
                // Drawn by their own functions before this dialog.
                Modal::Palette { .. } | Modal::Settings => {}
            }
        });
        if keep && !r.should_close() && self.modal.is_none() {
            self.modal = Some(m);
        } else if matches!(m, Modal::PgNewRole(_) | Modal::PgPassword(_)) {
            // Esc closes without the dialog's own buttons, so the password fields are forgotten here; dropping the form wipes them.
            super::sql::pg_roles::forget_pg_secrets(ctx, &m);
        } else if matches!(m, Modal::Conn(_)) {
            // A Test still running when the form closes must not show in the next form.
            self.conn_test = None;
            self.test_info = None;
        }
    }

    fn delete_dialog(&mut self, ui: &mut egui::Ui, at: &Sel, docs: &[Document], keep: &mut bool) {
        const SHOWN: usize = 5;
        let sql = self.conn(at.conn).is_some_and(|c| c.sql.is_some());
        let ids: Vec<String> = docs.iter().take(SHOWN).map(|d| d.get("_id").map(|k| if sql { k.as_document().map(|k| k.values().map(db::short).collect::<Vec<_>>().join("-")).unwrap_or_default() } else { db::short(k) }).unwrap_or_default()).collect();
        let more = if docs.len() > SHOWN { format!(" and {} more", docs.len() - SHOWN) } else { String::new() };
        let noun = if sql { "row" } else { "document" };
        ui.set_width(480.0);
        let title = if docs.len() == 1 { format!("Delete this {noun}?") } else { format!("Delete {} {noun}s?", docs.len()) };
        dialog_head(ui, &Head::new(title).sub(format!("{} on {}", at.ns(), self.conn_name(at.conn))).glyph(glyphs::trash).tone(ALERT()));
        dialog_body(ui, |ui| {
            code_card(ui, if sql { "Key" } else { "_id" }, |ui| ui.add(egui::Label::new(RichText::new(format!("{}{more}", ids.join(", "))).monospace().color(TEXT())).wrap()));
            ui.label(RichText::new("You can undo this for 24 hours.").size(TXT_SMALL).color(MUTED()));
        });
        dialog_foot(ui, |ui| {
            let del = danger_solid(ui, true, "Delete").clicked();
            // Run on the tab the rows were picked in: tabs can close or move while the dialog is open.
            if del && sql {
                let docs = docs.to_vec();
                self.on_tab(&at.clone(), |a| a.sql_delete(docs));
                *keep = false;
            } else if del {
                let docs = docs.to_vec();
                self.on_tab(&at.clone(), |a| {
                    if let Some((conn, target)) = a.writable_target() {
                        match docs.as_slice() {
                            [d] => a.worker.delete(conn, target, d.clone(), true),
                            _ => a.worker.delete_docs(conn, target, docs.clone()),
                        }
                        // The reload drops deleted rows from the checks; others stay checked.
                        if let Some(t) = a.tm() {
                            t.selected = None;
                        }
                    }
                });
                *keep = false;
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    fn new_collection_dialog(&mut self, ui: &mut egui::Ui, conn: &u64, db: &String, name: &mut String, view: &mut bool, source: &mut String, pipeline: &mut String, keep: &mut bool) {
        ui.set_width(if *view { 560.0 } else { 460.0 });
        let cname = self.conn_name(*conn);
        let head = if *view { Head::new("New view").sub(format!("in {db} on {cname}  ·  read-only, computed on each read")) } else { Head::new("New collection").sub(format!("in {db} on {cname}")) };
        dialog_head(ui, &head.glyph(glyphs::plus));
        dialog_body(ui, |ui| {
            if !*view {
                stacked_field(ui, "Name", "Created empty, with the default settings.", |ui| ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)));
                return;
            }
            let colls: &[String] = self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.as_deref()).unwrap_or_default();
            form_card(ui, "", "", |rows| {
                rows.label_width(120.0);
                rows.row("Name", "", |ui| ui.add(egui::TextEdit::singleline(name).desired_width(ui.available_width())));
                rows.row("Based on", "", |ui| {
                    egui::ComboBox::from_id_salt("view-src").width(ui.available_width()).selected_text(if source.is_empty() { "choose a collection" } else { source.as_str() }).show_ui(ui, |ui| {
                        menu_look(ui);
                        for c in colls {
                            ui.selectable_value(source, c.clone(), c.as_str());
                        }
                    });
                });
            });
            card_caption(ui, "Pipeline", "runs on every read of the view");
            let id = ui.make_persistent_id("view-pipeline");
            crate::ui::vim::wrap(ui, id, pipeline, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(6).layouter(&mut code_layout).show(ui)));
        });
        // Parsed once a frame, for both the footer note and Create.
        let parsed = view.then(|| db::parse_pipeline(pipeline));
        dialog_foot(ui, |ui| {
            let ok = !name.trim().is_empty() && (!*view || !source.is_empty());
            if ui.add_enabled(ok, primary_button(ui, if *view { "Create view" } else { "Create" }, PRIMARY())).clicked() {
                let cmd = match &parsed {
                    Some(p) => p.clone().map(|p| doc! { "create": name.trim(), "viewOn": source.as_str(), "pipeline": p }),
                    None => Ok(doc! { "create": name.trim() }),
                };
                match (cmd, self.conn(*conn).and_then(|c| c.client.clone())) {
                    (Ok(cmd), Some(client)) => {
                        self.worker.db_change(*conn, client, db.clone(), cmd, format!("Created {}", name.trim()));
                        *keep = false;
                    }
                    (Err(e), _) => self.err(e),
                    (_, None) => self.warn_offline(),
                }
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
            if let Some(p) = &parsed {
                match p {
                    Ok(_) => foot_note(ui, "Pipeline parses"),
                    Err(e) => {
                        foot_left(ui, |ui| ui.add(egui::Label::new(RichText::new(e).size(TXT_SMALL).color(ALERT())).truncate()));
                    }
                }
            }
        });
    }

    fn duplicate_coll_dialog(&mut self, ui: &mut egui::Ui, conn: &u64, db: &String, coll: &String, name: &mut String, keep: &mut bool) {
        ui.set_width(480.0);
        let cname = self.conn_name(*conn);
        dialog_head(ui, &Head::new(format!("Duplicate {coll}")).sub(format!("in {db} on {cname}")).glyph(glyphs::copy));
        let to = name.trim().to_string();
        let taken = self.copying.iter().any(|c| c.0 == *conn && c.1 == *db && c.2 == to) || self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.as_ref()).is_some_and(|l| l.iter().any(|n| *n == to));
        dialog_body(ui, |ui| {
            stacked_field(ui, "New collection", "Copies every document, index and setting (validator, collation).", |ui| ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)));
            if taken {
                field_note(ui, &format!("{db}.{to} already exists."), ALERT());
            }
        });
        dialog_foot(ui, |ui| {
            if ui.add_enabled(!to.is_empty() && !taken, primary_button(ui, "Duplicate", PRIMARY())).clicked() {
                match self.conn(*conn).and_then(|c| c.client.clone()) {
                    Some(client) => {
                        // The sidebar shows it with a spinner until the copy ends.
                        self.copying.insert((*conn, db.clone(), to.clone()));
                        self.worker.duplicate_coll(*conn, client, db.clone(), coll.clone(), to.clone());
                    }
                    None => self.warn_offline(),
                }
                *keep = false;
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    fn save_all_dialog(&mut self, ui: &mut egui::Ui, keys: &[String], keep: &mut bool) {
        let sql = self.t().is_some_and(|t| t.sql.is_some());
        let n: usize = keys.iter().filter_map(|k| self.drafts.get(k)).map(|d| db::diff(&d.original, &d.current).len()).sum();
        ui.set_width(480.0);
        let (title, note) = if sql {
            (format!("Save {n} cell{} in {} row{}?", if n == 1 { "" } else { "s" }, keys.len(), if keys.len() == 1 { "" } else { "s" }), "They are written in one transaction. If a row changed on the server since you opened it, nothing is written and the edits stay staged.")
        } else {
            (format!("Save {} documents ({n} changes)?", keys.len()), "Each document is refused if someone else changed it since you opened it; its edits are kept.")
        };
        dialog_head(ui, &Head::new(title).sub(self.t().map(|t| t.sel.ns()).unwrap_or_default()).glyph(glyphs::check));
        dialog_body(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(note).color(TEXT())).wrap());
        });
        dialog_foot(ui, |ui| {
            if primary(ui, "Save all", PRIMARY()).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                if sql {
                    self.write_sql_drafts(keys.to_vec());
                } else {
                    for k in keys.iter() {
                        self.write_draft(k);
                    }
                }
                *keep = false;
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    fn grouped_dialog(&mut self, ui: &mut egui::Ui, sel: &Sel, path: &String, pipeline: &[Document], rows: &[Document], keep: &mut bool) {
        ui.set_width(600.0);
        let total: u64 = rows.iter().filter_map(|r| r.get("count").and_then(|c| c.as_i64().or_else(|| c.as_i32().map(i64::from)))).map(|c| c as u64).sum();
        let sql = self.is_sql(sel.conn);
        let noun = if sql { "rows" } else { "documents" };
        dialog_head(ui, &Head::new(format!("Distinct values of {path}")).sub(format!("{}  ·  {} values in {} {noun}", sel.coll, group(&rows.len().to_string()), group(&total.to_string()))).glyph(glyphs::list));
        dialog_pane(ui, |ui| {
            if rows.len() >= 1000 {
                ui.label(RichText::new("The first 1,000 values are shown.").size(TXT_SMALL).color(MUTED()));
                ui.add_space(6.0);
            }
            table_card(ui, |ui| {
                let w = ui.available_width();
                table_head(ui, &[("Value", 0.0), (">Count", 120.0)]);
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    for (i, r) in rows.iter().enumerate() {
                        if i > 0 {
                            table_sep(ui);
                        }
                        ui.allocate_ui_with_layout(egui::vec2(w, 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_min_height(28.0);
                            ui.add_space(12.0);
                            // A fixed width: a truncating label otherwise shrinks to a few letters.
                            ui.allocate_ui_with_layout(egui::vec2(w - 150.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_min_width(w - 150.0);
                                ui.add(egui::Label::new(cell_job(r.get("_id").unwrap_or(&Bson::Null))).truncate());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_space(12.0);
                                ui.label(RichText::new(r.get("count").map(db::compact).unwrap_or_default()).monospace().color(C_NUMBER()));
                            });
                        });
                    }
                });
            });
        });
        dialog_foot(ui, |ui| {
            // Values counted from pipeline or script results have no pipeline to reopen.
            // A SQL table has no pipeline to open.
            if !sql && ui.add_enabled(!pipeline.is_empty(), primary_button(ui, "Open in Aggregate", PRIMARY())).on_hover_text("Open this pipeline in a new tab").clicked() {
                let mut t = Tab::new(sel.clone());
                t.view = View::Aggregate;
                t.agg_txt = format!("[\n{}\n]", pipeline.iter().map(|s| format!("  {}", db::shell_text(&Bson::Document(s.clone())))).collect::<Vec<_>>().join(",\n"));
                // Next to the current tab, like the original's nextToActive.
                let at = (self.cur + 1).min(self.tabs.len());
                self.tabs.insert(at, t);
                self.set_cur(at);
                self.reset_doc_state();
                self.run_pipeline(pipeline.to_vec());
                *keep = false;
            }
            if ui.button("Close").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            foot_left(ui, |ui| {
                if ui.button("Copy").on_hover_text("Copy as JSON: value and count").clicked() {
                    ui.ctx().copy_text(db::to_edit_value(&Bson::Array(rows.iter().cloned().map(Bson::Document).collect())));
                }
            });
        });
    }

    fn rename_field_dialog(&mut self, ui: &mut egui::Ui, d: &Document, path: &[String], name: &mut String, keep: &mut bool) {
        let from = path.join(".");
        ui.set_width(480.0);
        dialog_head(ui, &Head::new(format!("Rename {from}")).sub(self.t().map(|t| t.sel.ns()).unwrap_or_default()).glyph(glyphs::doc));
        let check = rename_check(d, path, name);
        dialog_body(ui, |ui| {
            stacked_field(ui, "New name", "", |ui| ui.add(egui::TextEdit::singleline(name).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)));
            if let Err(Some(msg)) = &check {
                field_note(ui, msg, ALERT());
            }
        });
        let valid = check.is_ok();
        let to = check.unwrap_or_default();
        dialog_foot(ui, |ui| {
            if ui.add_enabled(valid, primary_button(ui, "In this document", PRIMARY())).clicked() {
                let new = db::get_path(d, &from).cloned().ok_or_else(|| "Field not found".to_string()).and_then(|v| db::edit_path(&db::edit_path(d, path, None)?, &to, Some(v)));
                let base = d.clone();
                self.stage(&base, new);
                *keep = false;
            }
            // $rename cannot reach into arrays, so this is offered only for plain paths.
            let plain = path.iter().all(|s| s.parse::<usize>().is_err());
            let (reach, scope) = self.t().map(|t| (t.bulk_count(), t.query.scope())).unwrap_or_default();
            if plain && ui.add_enabled(valid, egui::Button::new("In all matching documents…")).clicked() {
                let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                self.modal = Some(Modal::Typed {
                    title: format!("Rename {from} to {} in every document of {coll} matching the current filter ({reach})? Documents that already have {} are skipped.", to.join("."), to.join(".")),
                    expect: coll,
                    typed: String::new(),
                    action: Danger::RenameMany { scope, from: from.clone(), to: to.join(".") },
                });
                *keep = false;
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    fn update_many_dialog(&mut self, ui: &mut egui::Ui, set_txt: &mut String, unset_txt: &mut String, preview: &mut Option<(Vec<Document>, db::Res<(Vec<(Document, Document)>, u64)>)>, keep: &mut bool) {
        ui.set_width(620.0);
        let scope = self.t().map(|t| t.query.scope()).unwrap_or_default();
        dialog_head(ui, &Head::new("Update matching documents").sub(self.t().map(|t| format!("{}  ·  every document matching the current filter", t.sel.ns())).unwrap_or_default()).glyph(glyphs::pencil));
        let stages = db::parse_doc(set_txt).map(|set| db::update_stages(set, unset_txt.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()));
        dialog_body(ui, |ui| {
            let mut changed = false;
            form_card(ui, "", "", |rows| {
                rows.stacked("Set fields", |ui| {
                    ui.label(RichText::new("Values starting with $ are read as field paths.").size(TXT_SMALL).color(MUTED()));
                    let id = ui.make_persistent_id("um-set");
                    changed |= crate::ui::vim::wrap(ui, id, set_txt, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(4).layouter(&mut code_layout).show(ui))).response.changed();
                });
                rows.stacked("Remove fields", |ui| {
                    changed |= ui.add(egui::TextEdit::singleline(unset_txt).font(egui::TextStyle::Monospace).hint_text("comma separated paths").desired_width(f32::INFINITY)).changed();
                });
            });
            if changed {
                *preview = None;
            }
            match &stages {
                Err(err) => field_note(ui, err, ALERT()),
                Ok(st) if st.is_empty() => field_note(ui, "Nothing to change yet.", MUTED()),
                Ok(_) => {}
            }
        });
        let ready = stages.as_ref().is_ok_and(|s| !s.is_empty());
        let fresh = matches!((preview.as_ref(), stages.as_ref()), (Some((p, _)), Ok(s)) if p == s);
        if let Some(r) = preview.as_ref().filter(|_| fresh).map(|(_, r)| r) {
            // The preview sits under the form, in the same body.
            dialog_pane(ui, |ui| match r {
                Err(err) => banner(ui, Notice::Danger, "Preview failed.", err, None),
                Ok((rows, n)) => {
                    card_caption(ui, "Preview", &format!("{} documents match, {} shown after the change", group(&n.to_string()), rows.len()));
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for (i, (old, new)) in rows.iter().enumerate() {
                            ui.label(RichText::new(old.get("_id").map(db::short).unwrap_or_default()).monospace().color(BRIGHT()));
                            diff_table(ui, &format!("um-chg-{i}"), db::diff(old, new).into_iter());
                        }
                    });
                    false
                }
            });
        }
        dialog_foot(ui, |ui| {
            let applicable = ready && fresh && matches!(preview, Some((_, Ok(_))));
            if ui.add_enabled(applicable, primary_button(ui, "Apply…", ALERT())).clicked() {
                let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                let n = if let Some((_, Ok((_, n)))) = preview { *n } else { 0 };
                self.modal = Some(Modal::Typed {
                    title: format!("Update every document in {coll} matching the current filter ({n})?"),
                    expect: coll,
                    typed: String::new(),
                    action: Danger::UpdateMany { scope: scope.clone(), stages: stages.clone().unwrap_or_default() },
                });
                *keep = false;
            }
            if ui.add_enabled(ready, egui::Button::new("Preview")).clicked() && let (Ok(st), Some(t)) = (&stages, self.target()) {
                self.worker.update_preview(t, scope.clone(), st.clone());
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    fn undo_dialog(&mut self, ui: &mut egui::Ui, keep: &mut bool) {
        ui.set_width(600.0);
        dialog_head(ui, &Head::new("Recent changes").sub(format!("The last {} actions from the past 24 hours", db::UNDO_MAX)).glyph(glyphs::undo));
        let mut pick = None;
        dialog_pane(ui, |ui| {
            if self.undo.is_empty() {
                ui.label(RichText::new("Nothing to undo.").color(MUTED()));
                return;
            }
            table_card(ui, |ui| {
                let w = ui.available_width();
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    for (i, a) in self.undo.iter().enumerate() {
                        if i > 0 {
                            table_sep(ui);
                        }
                        let what = match &a.kind {
                            UndoKind::Insert { id } => format!("inserted {}", db::short(id)),
                            UndoKind::InsertMany { ids } => format!("imported {} documents", ids.len()),
                            UndoKind::Delete { old } => format!("deleted {}", old.get("_id").map(db::short).unwrap_or_default()),
                            UndoKind::DeleteMany { old } => format!("deleted {} documents", old.len()),
                            UndoKind::Update { old, .. } => format!("edited {}", old.get("_id").map(db::short).unwrap_or_default()),
                            UndoKind::Sql { undo, .. } => undo.describe(),
                        };
                        let mins = a.at.elapsed().as_secs() / 60;
                        ui.allocate_ui_with_layout(egui::vec2(w, 40.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_min_height(40.0);
                            ui.add_space(12.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(RichText::new(what).color(BRIGHT()));
                                ui.label(RichText::new(format!("{}{}{}  ·  {mins} min ago", a.db, if a.coll.is_empty() { "" } else { "." }, a.coll)).size(TXT_SMALL).color(MUTED()));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_space(10.0);
                                let undo = ui.button("Undo");
                                undo.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Undo change {}", i + 1)));
                                if undo.clicked() {
                                    pick = Some(i);
                                }
                            });
                        });
                    }
                });
            });
        });
        if let Some(i) = pick {
            self.apply_undo(i);
        }
        if dialog_foot(ui, |ui| ui.button("Close").clicked()) {
            *keep = false;
        }
    }
}

/// The field a type-to-confirm dialog checks, labeled for tests and screen readers as "Confirmation".
pub(super) fn typed_confirm(ui: &mut egui::Ui, expect: &str, typed: &mut String) {
    stacked_field(ui, &format!("Type {expect} to confirm"), "Case sensitive.", |ui| {
        ui.add(egui::TextEdit::singleline(typed).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Confirmation"));
    });
}

/// Changed fields with the old value struck through and the new one after it, in a table card.
fn diff_table(ui: &mut egui::Ui, id: &str, rows: impl Iterator<Item = (String, Option<Bson>, Option<Bson>)>) {
    let show = |v: &Option<Bson>| v.as_ref().map(|v| format!("{} ({})", leaf_text(v), db::type_name(v))).unwrap_or("(none)".into());
    table_card(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(id).max_height(320.0).show(ui, |ui| {
                egui::Grid::new(id).num_columns(3).striped(true).spacing([18.0, 8.0]).show(ui, |ui| {
                    for h in ["FIELD", "BEFORE", "AFTER"] {
                        caption(ui, h);
                    }
                    ui.end_row();
                    for (path, a, b) in rows {
                        ui.label(RichText::new(path).monospace().color(BRIGHT()));
                        ui.label(RichText::new(show(&a)).monospace().color(ALERT()).strikethrough());
                        ui.label(RichText::new(show(&b)).monospace().color(SUCCESS()));
                        ui.end_row();
                    }
                });
            });
        });
    });
}


/// The new path for renaming the field at `path` to `name`. `Err(None)` disables the rename
/// quietly (empty, unchanged or not a plain name); `Err(Some(_))` says why.
fn rename_check(d: &Document, path: &[String], name: &str) -> Result<Vec<String>, Option<String>> {
    let name = name.trim();
    if name.is_empty() || name.contains('.') || name.starts_with('$') || Some(name) == path.last().map(|s| s.as_str()) {
        return Err(None);
    }
    let mut to = path[..path.len() - 1].to_vec();
    to.push(name.to_string());
    // Both the edit and $rename would replace the sibling's value with this one.
    if db::get_path(d, &to.join(".")).is_some() {
        return Err(Some(format!("{name} already exists here.")));
    }
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_never_overwrites_a_sibling() {
        let d = doc! { "a": 1, "b": 2, "o": { "x": 1, "y": 2 } };
        let p = |s: &str| s.split('.').map(String::from).collect::<Vec<_>>();
        assert_eq!(rename_check(&d, &p("a"), " c "), Ok(p("c")));
        assert_eq!(rename_check(&d, &p("o.x"), "z"), Ok(p("o.z")));
        assert_eq!(rename_check(&d, &p("a"), "b"), Err(Some("b already exists here.".into())));
        assert_eq!(rename_check(&d, &p("o.x"), "y"), Err(Some("y already exists here.".into())));
        for bad in ["", "a", "c.d", "$c"] {
            assert_eq!(rename_check(&d, &p("a"), bad), Err(None), "{bad:?}");
        }
    }
}
