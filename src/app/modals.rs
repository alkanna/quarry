//! The dialog layer: the JSON editor, insert, and the confirmations and pickers in `Modal`.
//! Larger dialogs live with their feature and are called from here.

use super::*;

impl App {
    pub(super) fn modals(&mut self, ctx: &egui::Context) {
        self.value_dialog(ctx);
        if let Some(text) = &mut self.insert {
            let mut close = false;
            let mut submit = false;
            let r = dialog(egui::Id::unique(("quarry-layer", "insert"))).show(ctx, |ui| {
                ui.set_width(560.0);
                dialog_title(ui, "Insert a document");
                ui.label(RichText::new("JSON, JSON5 or shell syntax: ObjectId(\"…\"), ISODate(\"…\"), NumberLong(…).").color(MUTED()));
                let id = egui::Id::unique("insert-editor-text");
                let find = Find::bar(ui, id, text);
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    let out = with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(16).layouter(&mut find.layouter(true)).show(ui));
                    find.reveal(ui, text, &out);
                });
                dialog_actions(ui, |ui| {
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
        let r = dialog(egui::Id::unique(("quarry-layer", "modal"))).show(ctx, |ui| {
            ui.set_max_width(620.0);
            match &mut m {
                Modal::Conn(f) => self.conn_dialog(ui, f, &mut keep),
                Modal::Attach(f) => self.attach_dialog(ui, f, &mut keep),
                Modal::Unlock(id, then) => {
                    let name = self.conn(*id).map(|c| c.meta.name.clone()).unwrap_or_default();
                    dialog_title(ui, format!("Allow writes on \"{name}\"?"));
                    ui.label(RichText::new("Edits, inserts, deletes and index changes become possible until you lock it again or quit. Each one still asks first.").color(MUTED()));
                    dialog_actions(ui, |ui| {
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
                    dialog_title(ui, format!("Save {} change(s)?", changes.len()));
                    ui.label(RichText::new("Saving is refused if someone else changed this document since you opened it.").color(MUTED()));
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        egui::Grid::new("chg").num_columns(3).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                            for (path, a, b) in changes.iter() {
                                ui.label(RichText::new(path).monospace().color(BRIGHT()));
                                let show = |v: &Option<Bson>| v.as_ref().map(|v| format!("{} ({})", leaf_text(v), db::type_name(v))).unwrap_or("(none)".into());
                                ui.label(RichText::new(show(a)).monospace().color(ALERT()).strikethrough());
                                ui.label(RichText::new(show(b)).monospace().color(SUCCESS()));
                                ui.end_row();
                            }
                        });
                    });
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Save", PRIMARY()).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            self.write_draft(tag);
                            keep = false;
                        }
                        if ui.button("Keep editing").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Delete(at, docs) => {
                    const SHOWN: usize = 5;
                    let sql = self.conn(at.conn).is_some_and(|c| c.sql.is_some());
                    let ids: Vec<String> = docs.iter().take(SHOWN).map(|d| d.get("_id").map(|k| if sql { k.as_document().map(|k| k.values().map(db::short).collect::<Vec<_>>().join("-")).unwrap_or_default() } else { db::short(k) }).unwrap_or_default()).collect();
                    let more = if docs.len() > SHOWN { format!(" and {} more", docs.len() - SHOWN) } else { String::new() };
                    let noun = if sql { "row" } else { "document" };
                    dialog_title(ui, if docs.len() == 1 { format!("Delete this {noun}?") } else { format!("Delete {} {noun}s?", docs.len()) });
                    ui.label(RichText::new(format!("{} {}{more}. You can undo this for 24 hours.", if sql { "Key" } else { "_id" }, ids.join(", "))).monospace().color(MUTED()));
                    dialog_actions(ui, |ui| {
                        let del = primary(ui, "Delete", ALERT()).clicked();
                        // Run on the tab the rows were picked in: tabs can close or move while the dialog is open.
                        if del && sql {
                            let docs = docs.clone();
                            self.on_tab(&at.clone(), |a| a.sql_delete(docs));
                            keep = false;
                        } else if del {
                            let docs = docs.clone();
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
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Export(f) => self.export_dialog(ui, f, &mut keep),
                Modal::SqlImport(f) => self.sql_import_dialog(ui, f, &mut keep),
                Modal::SqlDump(f) => self.sql_dump_dialog(ui, f, &mut keep),
                Modal::SqlRunFile(f) => self.sql_run_file_dialog(ui, f, &mut keep),
                Modal::SqlSet(f) => self.sql_set_dialog(ui, f, &mut keep),
                Modal::SqlRow(f) => self.sql_row_dialog(ui, f, &mut keep),
                Modal::ColEdit(f) => self.col_dialog(ui, f, &mut keep),
                Modal::IdxEdit(f) => self.idx_dialog(ui, f, &mut keep),
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
                    ui.label(semibold(RichText::new(title.as_str()).color(BRIGHT())));
                    ui.label(RichText::new(format!("Type {expect} to confirm (case sensitive).")).color(MUTED()));
                    ui.add(egui::TextEdit::singleline(typed).font(egui::TextStyle::Monospace).desired_width(320.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Confirmation"));
                    dialog_actions(ui, |ui| {
                        let ok = typed == expect;
                        if ui.add_enabled(ok, primary_button(ui, "Confirm", ALERT())).clicked() {
                            keep = false;
                            let action = std::mem::replace(action, Danger::DropCollection);
                            self.danger(action);
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::NewCollection { conn, db, name, view, source, pipeline } => {
                    ui.set_width(520.0);
                    dialog_title(ui, if *view { format!("New view in {db}") } else { format!("New collection in {db}") });
                    egui::Grid::new("newcoll").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Name");
                        ui.add(egui::TextEdit::singleline(name).desired_width(380.0));
                        ui.end_row();
                        if *view {
                            form_label(ui, "Based on");
                            let colls: Vec<String> = self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.clone()).unwrap_or_default();
                            egui::ComboBox::from_id_salt("view-src").width(380.0).selected_text(if source.is_empty() { "choose a collection" } else { source.as_str() }).show_ui(ui, |ui| {
                                menu_look(ui);
                                for c in colls {
                                    ui.selectable_value(source, c.clone(), c);
                                }
                            });
                            ui.end_row();
                            form_label(ui, "Pipeline");
                            let id = ui.make_persistent_id("view-pipeline");
                            with_autopair(ui, id, pipeline, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(380.0).desired_rows(6).layouter(&mut code_layout).show(ui));
                            ui.end_row();
                        }
                    });
                    dialog_actions(ui, |ui| {
                        let ok = !name.trim().is_empty() && (!*view || !source.is_empty());
                        if ui.add_enabled(ok, primary_button(ui, "Create", PRIMARY())).clicked() {
                            let cmd = if *view {
                                db::parse_pipeline(pipeline).map(|p| doc! { "create": name.trim(), "viewOn": source.as_str(), "pipeline": p })
                            } else {
                                Ok(doc! { "create": name.trim() })
                            };
                            match (cmd, self.conn(*conn).and_then(|c| c.client.clone())) {
                                (Ok(cmd), Some(client)) => {
                                    self.worker.db_change(*conn, client, db.clone(), cmd, format!("Created {}", name.trim()));
                                    keep = false;
                                }
                                (Err(e), _) => self.err(e),
                                (_, None) => self.warn_offline(),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Tool(f) => self.tool_dialog(ui, f, &mut keep),
                Modal::Relations { conn, db } => self.relations_dialog(ui, conn, db, &mut keep),
                Modal::RenameColl { conn, db, coll, name } => {
                    dialog_title(ui, format!("Rename {db}.{coll}"));
                    ui.label(RichText::new("Open tabs of this collection are closed, as in the original.").color(MUTED()));
                    ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                    dialog_actions(ui, |ui| {
                        let ok = !name.trim().is_empty() && name.trim() != coll.as_str();
                        if ui.add_enabled(ok, primary_button(ui, "Rename", PRIMARY())).clicked() {
                            if let Some(client) = self.conn(*conn).and_then(|c| c.client.clone()) {
                                let (from, to) = (format!("{db}.{coll}"), format!("{db}.{}", name.trim()));
                                self.tabs.retain(|t| !(t.sel.conn == *conn && t.sel.db == *db && t.sel.coll == *coll));
                                self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                                self.worker.db_change(*conn, client, db.clone(), doc! { "renameCollection": from, "to": to }, format!("Renamed {coll} to {}", name.trim()));
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::DuplicateColl { conn, db, coll, name } => {
                    dialog_title(ui, format!("Duplicate {db}.{coll}"));
                    ui.label(RichText::new("Copies every document, index and setting (validator, collation) into a new collection of this database.").color(MUTED()));
                    ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                    let to = name.trim();
                    let taken = self.copying.iter().any(|c| c.0 == *conn && c.1 == *db && c.2 == to) || self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.as_ref()).is_some_and(|l| l.iter().any(|n| n == to));
                    if taken {
                        ui.label(RichText::new(format!("{db}.{to} already exists.")).color(ALERT()));
                    }
                    dialog_actions(ui, |ui| {
                        if ui.add_enabled(!to.is_empty() && !taken, primary_button(ui, "Duplicate", PRIMARY())).clicked() {
                            match self.conn(*conn).and_then(|c| c.client.clone()) {
                                Some(client) => {
                                    // The sidebar shows it with a spinner until the copy ends.
                                    self.copying.insert((*conn, db.clone(), to.to_string()));
                                    self.worker.duplicate_coll(*conn, client, db.clone(), coll.clone(), to.to_string());
                                }
                                None => self.warn_offline(),
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ImportInsert(n) => {
                    let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                    dialog_title(ui, format!("Insert {n} documents into {coll}?"));
                    ui.label(RichText::new("They are added as new documents. Undo removes exactly these documents again.").color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Import", PRIMARY()).clicked() {
                            if let Some((conn, target)) = self.writable_target() {
                                if let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                                    self.worker.import(conn, target, docs, None);
                                    self.ok("Importing…");
                                }
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ShellWrite(ops) => {
                    dialog_title(ui, "Execute this command?");
                    ui.label(RichText::new("Your command contains potentially destructive operations:").color(MUTED()));
                    ui.label(RichText::new(ops.join(", ")).monospace().color(ALERT()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Execute", ALERT()).clicked() {
                            self.start_shell();
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::AggWrite(p) => {
                    dialog_title(ui, "This pipeline writes data ($out or $merge)");
                    ui.label(RichText::new("It can replace or change a whole collection and cannot be undone here.").color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Run it", ALERT()).clicked() {
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
                Modal::SaveAll(keys) => {
                    let sql = self.t().is_some_and(|t| t.sql.is_some());
                    let n: usize = keys.iter().filter_map(|k| self.drafts.get(k)).map(|d| db::diff(&d.original, &d.current).len()).sum();
                    if sql {
                        dialog_title(ui, format!("Save {n} cell{} in {} row{}?", if n == 1 { "" } else { "s" }, keys.len(), if keys.len() == 1 { "" } else { "s" }));
                        ui.label(RichText::new("They are written in one transaction. If a row changed on the server since you opened it, nothing is written and the edits stay staged.").color(MUTED()));
                    } else {
                        dialog_title(ui, format!("Save {} documents ({n} changes)?", keys.len()));
                        ui.label(RichText::new("Each document is refused if someone else changed it since you opened it; its edits are kept.").color(MUTED()));
                    }
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Save all", PRIMARY()).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            if sql {
                                self.write_sql_drafts(keys.clone());
                            } else {
                                for k in keys.iter() {
                                    self.write_draft(k);
                                }
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Grouped { sel, path, pipeline, rows } => {
                    ui.set_width(560.0);
                    dialog_title(ui, format!("Distinct values for {}: {path}", sel.coll));
                    let total: u64 = rows.iter().filter_map(|r| r.get("count").and_then(|c| c.as_i64().or_else(|| c.as_i32().map(i64::from)))).map(|c| c as u64).sum();
                    ui.label(RichText::new(format!("{} values in {} documents{}", group(&rows.len().to_string()), group(&total.to_string()), if rows.len() >= 1000 { ", first 1000 values shown" } else { "" })).color(MUTED()));
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        egui::Grid::new("grouped").striped(true).num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                            caption(ui, "VALUE");
                            caption(ui, "COUNT");
                            ui.end_row();
                            for r in rows.iter() {
                                // A fixed width: a truncating label in a grid otherwise shrinks to a few letters.
                                ui.allocate_ui_with_layout(egui::vec2(400.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    ui.set_min_width(400.0);
                                    ui.add(egui::Label::new(cell_job(r.get("_id").unwrap_or(&Bson::Null))).truncate());
                                });
                                ui.label(RichText::new(r.get("count").map(db::compact).unwrap_or_default()).monospace().color(C_NUMBER()));
                                ui.end_row();
                            }
                        });
                    });
                    dialog_actions(ui, |ui| {
                        // Values counted from pipeline or script results have no pipeline to reopen.
                        if ui.add_enabled(!pipeline.is_empty(), primary_button(ui, "Open in Aggregate", PRIMARY())).on_hover_text("Open this pipeline in a new tab").clicked() {
                            let mut t = Tab::new(sel.clone());
                            t.view = View::Aggregate;
                            t.agg_txt = format!("[\n{}\n]", pipeline.iter().map(|s| format!("  {}", db::shell_text(&Bson::Document(s.clone())))).collect::<Vec<_>>().join(",\n"));
                            // Next to the current tab, like the original's nextToActive.
                            let at = (self.cur + 1).min(self.tabs.len());
                            self.tabs.insert(at, t);
                            self.set_cur(at);
                            self.reset_doc_state();
                            self.run_pipeline(pipeline.clone());
                            keep = false;
                        }
                        if ui.button("Copy").on_hover_text("Copy as JSON: value and count").clicked() {
                            ui.ctx().copy_text(db::to_edit_value(&Bson::Array(rows.iter().cloned().map(Bson::Document).collect())));
                        }
                        if ui.button("Close").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ColumnCustomize { key, agg, col, label, path } => {
                    dialog_title(ui, format!("Customize column {col}"));
                    form_label(ui, "Column name");
                    ui.add(egui::TextEdit::singleline(label).hint_text(col.as_str()).desired_width(360.0));
                    form_label(ui, "Field path");
                    ui.add(egui::TextEdit::singleline(path).font(egui::TextStyle::Monospace).desired_width(360.0));
                    let p = path.trim();
                    let valid = !p.starts_with('$') && !p.split('.').any(str::is_empty);
                    dialog_actions(ui, |ui| {
                        if ui.add_enabled(valid, primary_button(ui, "Save", PRIMARY())).clicked() {
                            let fields = self.t().map(|t| if *agg { t.agg_source().1.to_vec() } else { t.fields.clone() }).unwrap_or_default();
                            self.apply_col_acts(key, &fields, vec![ColAct::Customize { col: col.clone(), label: label.clone(), path: p.to_string() }]);
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
                    ui.set_width((text_w + 40.0).clamp(560.0, 820.0));
                    dialog_title(ui, title.as_str());
                    let find = Find::bar(ui, egui::Id::unique("text-modal"), body);
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        let mut text = body.as_str();
                        let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(*code)).show(ui);
                        find.reveal(ui, body, &out);
                    });
                    dialog_actions(ui, |ui| {
                        if ui.button("Copy").clicked() {
                            ui.ctx().copy_text(body.clone());
                        }
                        if ui.button("Close").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::CreateIndex(f) => keep = self.create_index_dialog(ui, f),
                Modal::Rename { doc: d, path, name } => {
                    let from = path.join(".");
                    dialog_title(ui, format!("Rename {from}"));
                    ui.add(egui::TextEdit::singleline(name).font(egui::TextStyle::Monospace).desired_width(320.0));
                    let check = rename_check(d, path, name);
                    if let Err(Some(msg)) = &check {
                        ui.label(RichText::new(msg).color(ALERT()));
                    }
                    let valid = check.is_ok();
                    let to = check.unwrap_or_default();
                    dialog_actions(ui, |ui| {
                        if ui.add_enabled(valid, primary_button(ui, "In this document", PRIMARY())).clicked() {
                            let new = db::get_path(d, &from).cloned().ok_or_else(|| "Field not found".to_string()).and_then(|v| db::edit_path(&db::edit_path(d, path, None)?, &to, Some(v)));
                            let base = d.clone();
                            self.stage(&base, new);
                            keep = false;
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
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::UpdateMany { set_txt, unset_txt, preview } => {
                    ui.set_width(560.0);
                    let scope = self.t().map(|t| t.query.scope()).unwrap_or_default();
                    dialog_title(ui, "Update matching documents");
                    ui.label(RichText::new("Applies to every document matching the current filter. Values starting with $ are read as field paths.").color(MUTED()));
                    ui.label(RichText::new("Set fields").color(FAINT()));
                    let id = ui.make_persistent_id("um-set");
                    let mut changed = with_autopair(ui, id, set_txt, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(4).layouter(&mut code_layout).show(ui)).response.changed();
                    ui.label(RichText::new("Remove fields (comma separated paths)").color(FAINT()));
                    changed |= ui.add(egui::TextEdit::singleline(unset_txt).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).changed();
                    if changed {
                        *preview = None;
                    }
                    let stages = db::parse_doc(set_txt).map(|set| db::update_stages(set, unset_txt.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()));
                    match &stages {
                        Err(err) => {
                            ui.label(RichText::new(err).color(ALERT()));
                        }
                        Ok(st) if st.is_empty() => {
                            ui.label(RichText::new("Nothing to change yet.").color(MUTED()));
                        }
                        Ok(_) => {}
                    }
                    let ready = stages.as_ref().is_ok_and(|s| !s.is_empty());
                    let fresh = matches!((preview.as_ref(), stages.as_ref()), (Some((p, _)), Ok(s)) if p == s);
                    match preview.as_ref().filter(|_| fresh).map(|(_, r)| r) {
                        Some(Err(err)) => {
                            ui.label(RichText::new(&*err).color(ALERT()));
                        }
                        Some(Ok((rows, n))) => {
                            ui.label(RichText::new(format!("{} documents match. Showing {} of them after the change.", group(&n.to_string()), rows.len())).color(MUTED()));
                            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                                for (i, (old, new)) in rows.iter().enumerate() {
                                    ui.label(RichText::new(old.get("_id").map(db::short).unwrap_or_default()).monospace().color(BRIGHT()));
                                    egui::Grid::new(("um-chg", i)).num_columns(3).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                                        for (path, a, b) in db::diff(old, new) {
                                            let show = |v: &Option<Bson>| v.as_ref().map(|v| format!("{} ({})", leaf_text(v), db::type_name(v))).unwrap_or("(none)".into());
                                            ui.label(RichText::new(path).monospace().color(KEY()));
                                            ui.label(RichText::new(show(&a)).monospace().color(ALERT()).strikethrough());
                                            ui.label(RichText::new(show(&b)).monospace().color(SUCCESS()));
                                            ui.end_row();
                                        }
                                    });
                                }
                            });
                        }
                        None => {}
                    }
                    dialog_actions(ui, |ui| {
                        if ui.add_enabled(ready, egui::Button::new("Preview")).clicked() {
                            if let (Ok(st), Some(t)) = (&stages, self.target()) {
                                self.worker.update_preview(t, scope.clone(), st.clone());
                            }
                        }
                        let applicable = ready && fresh && matches!(preview, Some((_, Ok(_))));
                        if ui.add_enabled(applicable, primary_button(ui, "Apply…", ALERT())).clicked() {
                            let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                            let n = if let Some((_, Ok((_, n)))) = preview { *n } else { 0 };
                            self.modal = Some(Modal::Typed {
                                title: format!("Update every document in {coll} matching the current filter ({n})?"),
                                expect: coll,
                                typed: String::new(),
                                action: Danger::UpdateMany { scope, stages: stages.unwrap_or_default() },
                            });
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Undo => {
                    dialog_title(ui, "Recent changes");
                    ui.label(RichText::new(format!("Last {} actions from the past 24 hours.", db::UNDO_MAX)).color(MUTED()));
                    let mut pick = None;
                    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                        for (i, a) in self.undo.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let what = match &a.kind {
                                    UndoKind::Insert { id } => format!("inserted {}", db::short(id)),
                                    UndoKind::InsertMany { ids } => format!("imported {} documents", ids.len()),
                                    UndoKind::Delete { old } => format!("deleted {}", old.get("_id").map(db::short).unwrap_or_default()),
                                    UndoKind::DeleteMany { old } => format!("deleted {} documents", old.len()),
                                    UndoKind::Update { old, .. } => format!("edited {}", old.get("_id").map(db::short).unwrap_or_default()),
                                    UndoKind::Sql { undo, .. } => undo.describe(),
                                };
                                let mins = a.at.elapsed().as_secs() / 60;
                                ui.label(RichText::new(format!("{}{}{}  {what}", a.db, if a.coll.is_empty() { "" } else { "." }, a.coll)).monospace());
                                ui.label(RichText::new(format!("{mins} min ago")).color(MUTED()));
                                let undo = ui.button("Undo");
                                undo.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Undo change {}", i + 1)));
                                if undo.clicked() {
                                    pick = Some(i);
                                }
                            });
                        }
                    });
                    if let Some(i) = pick {
                        self.apply_undo(i);
                    }
                    if dialog_actions(ui, |ui| ui.button("Close").clicked()) {
                        keep = false;
                    }
                }
                Modal::Explain { summary, raw, show_raw } => self.explain_dialog(ui, summary, raw, show_raw, &mut keep),
                // Drawn by their own functions before this dialog.
                Modal::Palette { .. } | Modal::Settings => {}
            }
        });
        if keep && !r.should_close() && self.modal.is_none() {
            self.modal = Some(m);
        } else if matches!(m, Modal::PgNewRole(_) | Modal::PgPassword(_)) {
            // Esc closes without the dialog's own buttons, so the password fields are forgotten here; dropping the form wipes them.
            super::pg_roles::forget_pg_secrets(ctx, &m);
        } else if matches!(m, Modal::Conn(_)) {
            // A Test still running when the form closes must not show in the next form.
            self.conn_test = None;
            self.test_info = None;
        }
    }
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
