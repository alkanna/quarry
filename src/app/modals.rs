//! Every dialog: confirmations, connection form, editors and pickers.

use super::*;

impl App {
    pub(crate) fn modals(&mut self, ctx: &egui::Context) {
        if let Some(mut je) = self.json_edit.take() {
            let mut apply = false;
            let mut close = false;
            let screen = ctx.content_rect();
            let r = dialog(egui::Id::unique(("quarry-layer", "jsonedit"))).show(ctx, |ui| {
                ui.set_width((screen.width() * 0.72).clamp(480.0, 1200.0));
                ui.horizontal(|ui| {
                    let title = if je.path.is_empty() { "Edit document".to_string() } else { format!("Edit {}", je.path.join(".")) };
                    dialog_title(ui, title);
                    if !je.path.is_empty() {
                        ui.add_space(8.0);
                        Self::type_combo(ui, "jsonkind", &mut je.kind);
                    }
                });
                ui.label(RichText::new("JSON, JSON5 or shell syntax. Applying keeps it as an unsaved edit until you press Save.").size(TXT_SMALL).color(MUTED()));
                ui.add_space(4.0);
                let rows = je.text.lines().count().clamp(12, 40);
                let id = egui::Id::unique("json-editor-text");
                let find = Find::bar(ui, id, &je.text);
                egui::ScrollArea::vertical().max_height(screen.height() * 0.62).show(ui, |ui| {
                    let out = with_autopair(ui, id, &mut je.text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(rows).layouter(&mut find.layouter(true)).show(ui));
                    find.reveal(ui, &je.text, &out);
                    if je.focus {
                        out.response.request_focus();
                        je.focus = false;
                    }
                });
                ui.add_space(4.0);
                dialog_actions(ui, |ui| {
                    apply = primary(ui, "Apply", PRIMARY()).on_hover_text("Cmd+Enter").clicked() || ui.input(|i| i.modifiers.command && (i.key_pressed(Key::Enter) || i.key_pressed(Key::S)));
                    close = ui.button("Cancel").on_hover_text("Esc").clicked();
                });
            });
            if apply {
                let new = if je.path.is_empty() {
                    db::parse_doc(&je.text).and_then(|mut new| {
                        match (je.base.get("_id"), new.get("_id")) {
                            (Some(a), Some(b)) if a != b => return Err("The _id of a document cannot change. Use Duplicate to make a copy.".to_string()),
                            (Some(a), None) => {
                                // Keep _id first, as MongoDB stores it.
                                let mut d = doc! { "_id": a.clone() };
                                d.extend(new);
                                new = d;
                            }
                            _ => {}
                        }
                        Ok(new)
                    })
                } else {
                    db::make_value(je.kind, &je.text).and_then(|v| db::edit_path(&je.base, &je.path, Some(v)))
                };
                match new {
                    Ok(n) => {
                        let base = je.base.clone();
                        self.stage(&base, Ok(n));
                    }
                    Err(e) => {
                        self.err(e);
                        self.json_edit = Some(je);
                    }
                }
            } else if !(close || r.should_close()) {
                self.json_edit = Some(je);
            }
        }
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

        let Some(mut m) = self.modal.take() else { return };
        let mut keep = true;
        let r = dialog(egui::Id::unique(("quarry-layer", "modal"))).show(ctx, |ui| {
            ui.set_max_width(620.0);
            match &mut m {
                Modal::Conn(f) => self.conn_dialog(ui, f, &mut keep),
                Modal::Settings => self.settings_dialog(ui, &mut keep),
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
                Modal::Delete(docs) => {
                    const SHOWN: usize = 5;
                    let ids: Vec<String> = docs.iter().take(SHOWN).map(|d| d.get("_id").map(db::short).unwrap_or_default()).collect();
                    let more = if docs.len() > SHOWN { format!(" and {} more", docs.len() - SHOWN) } else { String::new() };
                    dialog_title(ui, if docs.len() == 1 { "Delete this document?".to_string() } else { format!("Delete {} documents?", docs.len()) });
                    ui.label(RichText::new(format!("_id {}{more}. You can undo this for 24 hours.", ids.join(", "))).monospace().color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Delete", ALERT()).clicked() {
                            if let Some((conn, target)) = self.writable_target() {
                                match docs.as_slice() {
                                    [d] => self.worker.delete(conn, target, d.clone(), true),
                                    _ => self.worker.delete_docs(conn, target, docs.clone()),
                                }
                                // The reload drops deleted rows from the checks; others stay checked.
                                if let Some(t) = self.tm() {
                                    t.selected = None;
                                }
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Typed { title, expect, typed, action } => {
                    ui.label(RichText::new(title.as_str()).strong().color(BRIGHT()));
                    ui.label(RichText::new(format!("Type {expect} to confirm (case sensitive).")).color(MUTED()));
                    ui.add(egui::TextEdit::singleline(typed).font(egui::TextStyle::Monospace).desired_width(320.0));
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
                                (_, None) => self.err("Not connected."),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Tool(f) => {
                    let name = if f.coll.is_empty() { f.db.clone() } else { format!("{}.{}", f.db, f.coll) };
                    ui.set_width(560.0);
                    dialog_title(ui, if f.restore { format!("Restore into {name}") } else { format!("Dump {name}") });
                    let meta = self.conn(f.conn).map(|c| c.meta.clone()).unwrap_or_default();
                    let tool = if f.restore { "mongorestore" } else { "mongodump" };
                    let exe = find_exe(tool, if f.restore { &self.prefs.mongorestore } else { &self.prefs.mongodump });
                    ui.label(RichText::new(match &exe {
                        Some(p) => format!("Runs {} with this connection's address.", p.display()),
                        None => format!("{tool} was not found. Install the MongoDB Database Tools here, or set its path in Settings."),
                    }).size(TXT_SMALL).color(if exe.is_some() { MUTED() } else { WARNING() }));
                    if exe.is_none() {
                        self.install_tools_button(ui);
                    }
                    let tunneled = meta.ssh.is_some() || meta.proxy.is_some();
                    if tunneled {
                        ui.label(RichText::new("This connection goes through SSH or a proxy, which the MongoDB tools cannot use. Open an SSH port forward and use a direct connection instead.").color(WARNING()));
                    }
                    ui.add_space(6.0);
                    egui::Grid::new("toolform").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Archive file");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut f.path).desired_width(360.0));
                            if ui.button("Choose…").clicked() {
                                let d = rfd::FileDialog::new();
                                let picked = if f.restore { d.pick_file() } else { d.set_file_name(format!("{name}.archive.gz")).save_file() };
                                if let Some(p) = picked {
                                    f.path = p.display().to_string();
                                    if f.restore {
                                        f.gzip = f.path.ends_with(".gz");
                                    }
                                }
                            }
                        });
                        ui.end_row();
                        form_label(ui, "");
                        ui.checkbox(&mut f.gzip, "Compressed (gzip)");
                        ui.end_row();
                        if f.restore {
                            form_label(ui, "Database in the archive");
                            ui.add(egui::TextEdit::singleline(&mut f.from_db).desired_width(200.0)).on_hover_text("Restored under the name of this database when it differs");
                            ui.end_row();
                            form_label(ui, "Only collection");
                            ui.add(egui::TextEdit::singleline(&mut f.coll).hint_text("all").desired_width(200.0));
                            ui.end_row();
                            form_label(ui, "");
                            let col = if f.drop { ALERT() } else { TEXT() };
                            ui.checkbox(&mut f.drop, RichText::new("Drop each collection before restoring it").color(col));
                            ui.end_row();
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let ok = exe.is_some() && !tunneled && !f.path.trim().is_empty() && (!f.restore || !f.from_db.trim().is_empty());
                        if ui.add_enabled(ok, primary_button(ui, if f.restore { "Restore…" } else { "Dump" }, if f.restore { WARNING() } else { PRIMARY() })).clicked() {
                            match stored_uri(f.conn) {
                                Err(e) => self.err(uri_err(e)),
                                Ok(uri) => {
                                    let coll = Some(f.coll.trim()).filter(|c| !c.is_empty());
                                    let path = f.path.trim();
                                    if f.restore {
                                        let args = db::restore_args(&uri, path, f.from_db.trim(), coll, &f.db, f.drop, f.gzip);
                                        let what = if f.drop { "Existing collections with the same names are dropped first. " } else { "" };
                                        self.modal = Some(Modal::Typed { title: format!("Restore {path} into {}? {what}Type the database name.", f.db), expect: f.db.clone(), typed: String::new(), action: Danger::Restore { conn: f.conn, args } });
                                    } else {
                                        self.start_tool(false, format!("Dump {name}"), db::dump_args(&uri, &f.db, coll, path, f.gzip));
                                    }
                                    keep = false;
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
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
                    let n: usize = keys.iter().filter_map(|k| self.drafts.get(k)).map(|d| db::diff(&d.original, &d.current).len()).sum();
                    dialog_title(ui, format!("Save {} documents ({n} changes)?", keys.len()));
                    ui.label(RichText::new("Each document is refused if someone else changed it since you opened it; its edits are kept.").color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Save all", PRIMARY()).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            for k in keys.iter() {
                                self.write_draft(k);
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
                        if primary(ui, "Open in Aggregate", PRIMARY()).on_hover_text("Open this pipeline in a new tab").clicked() {
                            let mut t = Tab::new(sel.clone());
                            t.view = View::Aggregate;
                            t.agg_txt = format!("[\n{}\n]", pipeline.iter().map(|s| format!("  {}", db::shell_text(&Bson::Document(s.clone())))).collect::<Vec<_>>().join(",\n"));
                            // Next to the current tab, like the original's nextToActive.
                            let at = (self.cur + 1).min(self.tabs.len());
                            self.tabs.insert(at, t);
                            self.cur = at;
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
                Modal::Text { title, body, code } => {
                    ui.set_width(560.0);
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
                Modal::Rename { doc: d, path, name } => {
                    let from = path.join(".");
                    dialog_title(ui, format!("Rename {from}"));
                    ui.add(egui::TextEdit::singleline(name).font(egui::TextStyle::Monospace).desired_width(320.0));
                    let mut valid = !name.trim().is_empty() && !name.contains('.') && !name.starts_with('$') && Some(name.trim()) != path.last().map(|s| s.as_str());
                    let mut to = path[..path.len() - 1].to_vec();
                    to.push(name.trim().to_string());
                    // Both the edit and $rename would replace the sibling's value with this one.
                    if valid && db::get_path(d, &to.join(".")).is_some() {
                        ui.label(RichText::new(format!("{} already exists here.", name.trim())).color(ALERT()));
                        valid = false;
                    }
                    dialog_actions(ui, |ui| {
                        if ui.add_enabled(valid, primary_button(ui, "In this document", PRIMARY())).clicked() {
                            let new = db::get_path(d, &from).cloned().ok_or_else(|| "Field not found".to_string()).and_then(|v| db::edit_path(&db::edit_path(d, path, None)?, &to, Some(v)));
                            let base = d.clone();
                            self.stage(&base, new);
                            keep = false;
                        }
                        // $rename cannot reach into arrays, so this is offered only for plain paths.
                        let plain = path.iter().all(|s| s.parse::<usize>().is_err());
                        let (count, filter) = self.t().map(|t| (t.count, t.query.filter.clone())).unwrap_or_default();
                        if plain && ui.add_enabled(valid, egui::Button::new(format!("In all {count} matching documents…"))).clicked() {
                            let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                            self.modal = Some(Modal::Typed {
                                title: format!("Rename {from} to {} in every document of {coll} matching the current filter (about {count})?", to.join(".")),
                                expect: coll,
                                typed: String::new(),
                                action: Danger::RenameMany { filter, from: from.clone(), to: to.join(".") },
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
                    let filter = self.t().map(|t| t.query.filter.clone()).unwrap_or_default();
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
                                self.worker.update_preview(t, filter.clone(), st.clone());
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
                                action: Danger::UpdateMany { filter, stages: stages.unwrap_or_default() },
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
                                };
                                let mins = a.at.elapsed().as_secs() / 60;
                                ui.label(RichText::new(format!("{}.{}  {what}", a.db, a.coll)).monospace());
                                ui.label(RichText::new(format!("{mins} min ago")).color(MUTED()));
                                if ui.button("Undo").clicked() {
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
                Modal::Explain { summary, raw, show_raw } => {
                    ui.set_width(720.0);
                    dialog_title(ui, "Explain");
                    ui.label(RichText::new(format!("{} returned  ·  {} keys examined  ·  {} documents examined  ·  {} ms", group(&summary.returned.to_string()), group(&summary.keys.to_string()), group(&summary.docs.to_string()), summary.ms)).color(MUTED()));
                    for w in &summary.warnings {
                        ui.label(RichText::new(w.as_str()).color(WARNING()));
                    }
                    ui.horizontal(|ui| {
                        segmented(ui, show_raw, &[(false, "Stages"), (true, "Raw JSON")]);
                    });
                    let find = if *show_raw { Find::bar(ui, egui::Id::unique("explain-raw"), raw) } else { Find::default() };
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        if *show_raw {
                            let mut text = raw.as_str();
                            let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(true)).show(ui);
                            find.reveal(ui, raw, &out);
                        } else if summary.rows.is_empty() {
                            ui.label(RichText::new("The server returned no execution stages. See Raw JSON.").color(MUTED()));
                        } else {
                            egui::Grid::new("explain-stages").num_columns(6).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                                for h in ["STAGE", "INDEX", "KEYS", "DOCS", "RETURNED", "MS"] {
                                    caption(ui, h);
                                }
                                ui.end_row();
                                let n = |v: Option<i64>| v.map(|v| group(&v.to_string())).unwrap_or_default();
                                for r in &summary.rows {
                                    let c = match r.stage.to_lowercase().as_str() {
                                        "ixscan" | "ixseek" => SUCCESS(),
                                        "collscan" | "scan" => ALERT(),
                                        _ => TEXT(),
                                    };
                                    ui.label(RichText::new(format!("{}{}", "  ".repeat(r.depth), r.stage)).monospace().color(c));
                                    ui.label(RichText::new(r.index.as_str()).monospace().color(MUTED()));
                                    for v in [r.keys, r.docs, r.returned, r.ms] {
                                        ui.label(RichText::new(n(v)).monospace());
                                    }
                                    ui.end_row();
                                }
                            });
                        }
                    });
                    dialog_actions(ui, |ui| {
                        if ui.button("Copy raw").clicked() {
                            ui.ctx().copy_text(raw.clone());
                        }
                        if ui.button("Close").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Palette { query, sel } => {
                    ui.set_width(560.0);
                    // Taken before the text field sees them, so arrows move the selection and Enter picks.
                    let (up, down, enter) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, Key::Enter)));
                    let r = ui.add(egui::TextEdit::singleline(query).hint_text("Jump to a connection, database or collection").desired_width(f32::INFINITY).margin(egui::Margin::symmetric(10, 8)));
                    r.request_focus();
                    if r.changed() {
                        *sel = 0;
                    }
                    let items = jumps(&self.conns, self.active, query);
                    if down {
                        *sel = (*sel + 1).min(items.len().saturating_sub(1));
                    }
                    if up {
                        *sel = sel.saturating_sub(1);
                    }
                    let mut pick = if enter { items.get(*sel).map(|x| x.2.clone()) } else { None };
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        if items.is_empty() {
                            ui.label(RichText::new("No matches").color(MUTED()));
                        }
                        for (i, (label, hint, j)) in items.iter().enumerate() {
                            let r = ui
                                .horizontal(|ui| {
                                    let r = ui.selectable_label(i == *sel, RichText::new(label).color(BRIGHT()));
                                    ui.label(RichText::new(hint).size(TXT_SMALL).color(FAINT()));
                                    r
                                })
                                .inner;
                            if i == *sel && (up || down) {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                pick = Some(j.clone());
                            }
                        }
                    });
                    if let Some(j) = pick {
                        keep = false;
                        self.jump(j);
                    }
                }
            }
        });
        if keep && !r.should_close() && self.modal.is_none() {
            self.modal = Some(m);
        }
    }

    fn conn_dialog(&mut self, ui: &mut egui::Ui, f: &mut ConnForm, keep: &mut bool) {
        let mut save = false;
        let mut test = false;
        ui.set_width(600.0);
        dialog_title(ui, if f.id.is_some() { "Edit connection" } else { "New connection" });
        ui.label(RichText::new("Addresses, passwords and passphrases are stored in the macOS Keychain. The connection opens read-only.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() - 220.0).show(ui, |ui| {
            settings_section(ui, "SERVER", |ui| {
                // Grid cells do not know their width on the first frame, so size fields from the section.
                let field_w = ui.available_width() - FORM_LABEL_W - 14.0;
                egui::Grid::new("cf").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                    form_label(ui, "Name");
                    ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Production cluster").desired_width(field_w).margin(egui::Margin::symmetric(8, 5)));
                    ui.end_row();
                    form_label(ui, "Tag");
                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for t in std::iter::once("").chain(TAG_PRESETS) {
                                let c = if t.is_empty() { MUTED() } else { tag_color(t) };
                                if chip(ui, if t.is_empty() { "None" } else { t }, c, clean_tag(&f.tag).eq_ignore_ascii_case(t)).clicked() {
                                    f.tag = t.to_string();
                                }
                            }
                        });
                        ui.add_space(4.0);
                        ui.add(egui::TextEdit::singleline(&mut f.tag).hint_text("or type a custom tag").char_limit(24).desired_width(220.0).margin(egui::Margin::symmetric(8, 5)));
                    });
                    ui.end_row();
                    form_label(ui, "URI");
                    ui.vertical(|ui| {
                        let edit = if f.show { egui::TextEdit::multiline(&mut f.uri).desired_rows(3).return_key(None) } else { egui::TextEdit::singleline(&mut f.uri).password(true) };
                        ui.add(edit.font(egui::TextStyle::Monospace).hint_text("mongodb+srv://user:password@host/").desired_width(field_w).margin(egui::Margin::symmetric(8, 6)));
                        // Wrapping is only visual; a pasted newline would break the URI.
                        if f.show {
                            f.uri.retain(|c| c != '\n' && c != '\r');
                        }
                        ui.add_space(2.0);
                        ui.checkbox(&mut f.show, "Show address");
                    });
                    ui.end_row();
                });
            });
            settings_section(ui, "SSH TUNNEL", |ui| {
                ui.checkbox(&mut f.ssh_on, "Connect through an SSH server (jump host)");
                if f.ssh_on {
                    egui::Grid::new("ssh").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Host");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut f.ssh.host).hint_text("bastion.example.com").desired_width(280.0));
                            ui.label("Port");
                            let mut port = if f.ssh.port == 0 { "22".to_string() } else { f.ssh.port.to_string() };
                            if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                                f.ssh.port = port.trim().parse().unwrap_or(22);
                            }
                        });
                        ui.end_row();
                        form_label(ui, "User");
                        ui.add(egui::TextEdit::singleline(&mut f.ssh.user).desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Log in with");
                        segmented(ui, &mut f.ssh.auth, &[(crate::ssh::SshAuth::Auto, "Auto"), (crate::ssh::SshAuth::Password, "Password"), (crate::ssh::SshAuth::KeyFile, "Key file"), (crate::ssh::SshAuth::Agent, "SSH agent")]);
                        ui.end_row();
                        use crate::ssh::SshAuth::*;
                        if matches!(f.ssh.auth, Auto | Password) {
                            form_label(ui, "Password");
                            ui.add(egui::TextEdit::singleline(&mut f.ssh_pwd).password(true).hint_text(if f.ssh.auth == Auto { "optional, tried last" } else { "" }).desired_width(280.0));
                            ui.end_row();
                        }
                        if matches!(f.ssh.auth, Auto | KeyFile) {
                            form_label(ui, "Key file");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut f.ssh.key_path).hint_text(if f.ssh.auth == Auto { "optional; ~/.ssh/id_* are tried" } else { "~/.ssh/id_ed25519" }).desired_width(280.0));
                                if ui.button("Choose…").clicked() {
                                    if let Some(p) = rfd::FileDialog::new().set_directory(dirs::home_dir().unwrap_or_default().join(".ssh")).pick_file() {
                                        f.ssh.key_path = p.display().to_string();
                                    }
                                }
                            });
                            ui.end_row();
                            form_label(ui, "Passphrase");
                            ui.add(egui::TextEdit::singleline(&mut f.ssh_pass).password(true).hint_text("if the key has one").desired_width(280.0));
                            ui.end_row();
                        }
                        ui.label("");
                        ui.checkbox(&mut f.ssh.legacy_rsa, "Allow legacy ssh-rsa host keys (old servers)");
                        ui.end_row();
                        form_label(ui, "Host key");
                        ui.horizontal(|ui| match f.ssh.known_host.clone() {
                            Some(fp) => {
                                ui.label(RichText::new(fp).monospace().size(TXT_SMALL).color(MUTED()));
                                if ui.button("Forget").on_hover_text("Trust the next key the server shows, e.g. after a reinstall").clicked() {
                                    f.ssh.known_host = None;
                                }
                            }
                            None => {
                                ui.label(RichText::new("remembered on first connect; a different key later is refused").size(TXT_SMALL).color(FAINT()));
                            }
                        });
                        ui.end_row();
                    });
                }
            });
            settings_section(ui, "SOCKS5 PROXY", |ui| {
                ui.add_enabled(!f.ssh_on, egui::Checkbox::new(&mut f.proxy_on, "Connect through a SOCKS5 proxy"));
                if f.ssh_on {
                    ui.label(RichText::new("Not needed with an SSH tunnel: the tunnel already carries the connection.").size(TXT_SMALL).color(FAINT()));
                } else if f.proxy_on {
                    egui::Grid::new("px").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Host");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut f.proxy.host).desired_width(280.0));
                            ui.label("Port");
                            let mut port = if f.proxy.port == 0 { "1080".to_string() } else { f.proxy.port.to_string() };
                            if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                                f.proxy.port = port.trim().parse().unwrap_or(1080);
                            }
                        });
                        ui.end_row();
                        form_label(ui, "User");
                        ui.add(egui::TextEdit::singleline(&mut f.proxy.user).hint_text("optional").desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Password");
                        ui.add(egui::TextEdit::singleline(&mut f.proxy_pwd).password(true).desired_width(280.0));
                        ui.end_row();
                    });
                }
            });
        });
        match &self.conn_test {
            None if f.testing => {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Testing…").color(MUTED()));
                });
            }
            Some(Ok(m)) => {
                ui.label(RichText::new(m).color(SUCCESS()));
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(e).color(ALERT())).wrap());
            }
            None => {}
        }
        dialog_actions(ui, |ui| {
            let valid = !f.name.trim().is_empty() && (f.uri.starts_with("mongodb://") || f.uri.starts_with("mongodb+srv://")) && (!f.ssh_on || (!f.ssh.host.trim().is_empty() && !f.ssh.user.trim().is_empty())) && (!f.proxy_on || f.ssh_on || !f.proxy.host.trim().is_empty());
            save = ui.add_enabled(valid, primary_button(ui, "Save and connect", PRIMARY())).clicked();
            test = ui.add_enabled(valid, egui::Button::new("Test")).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if test {
            f.testing = true;
            self.conn_test = None;
            let net = f.net();
            self.worker.test(f.uri.trim().to_string(), self.timeout(), net);
        }
        if save {
            let id = f.id.unwrap_or_else(|| self.conns.iter().map(|c| c.meta.id).max().unwrap_or(0) + 1);
            // Demo ids overlap real ones, so the demo keeps its edits in memory only.
            let saved = if self.demo { Some(Ok(())) } else { keychain(id).map(|k| k.set_password(f.uri.trim())) };
            match saved {
                Some(Ok(())) => {
                    let stored = if self.demo {
                        Vec::new()
                    } else {
                        vec![
                            set_secret(id, "ssh-password", if f.ssh_on { &f.ssh_pwd } else { "" }),
                            set_secret(id, "ssh-passphrase", if f.ssh_on { &f.ssh_pass } else { "" }),
                            set_secret(id, "proxy-password", if f.proxy_on && !f.ssh_on { &f.proxy_pwd } else { "" }),
                        ]
                    };
                    let meta = ConnMeta {
                        id,
                        name: f.name.trim().into(),
                        tag: clean_tag(&f.tag).into(),
                        ssh: f.ssh_on.then(|| crate::ssh::SshConf { host: f.ssh.host.trim().into(), user: f.ssh.user.trim().into(), ..f.ssh.clone() }),
                        proxy: (f.proxy_on && !f.ssh_on).then(|| db::Proxy { host: f.proxy.host.trim().into(), password: None, ..f.proxy.clone() }),
                    };
                    match self.conns.iter_mut().find(|c| c.meta.id == id) {
                        Some(c) => {
                            c.meta = meta;
                            c.masked_uri = db::mask_uri(f.uri.trim());
                            c.writable = false;
                            c.dbs.clear();
                        }
                        None => self.conns.push(Conn { meta, client: None, status: Status::Idle, writable: false, dbs: Vec::new(), masked_uri: db::mask_uri(f.uri.trim()), tunnel: None }),
                    }
                    self.active = Some(id);
                    self.save_conns();
                    self.connect(id);
                    *keep = false;
                    if let Some(Err(e)) = stored.into_iter().find(|r| r.is_err()) {
                        self.err(e);
                    }
                }
                Some(Err(e)) => self.err(format!("Could not save the address to the Keychain: {e}")),
                None => self.err("The Keychain is not available."),
            }
        }
    }

    fn settings_dialog(&mut self, ui: &mut egui::Ui, keep: &mut bool) {
        ui.set_width(600.0);
        dialog_title(ui, "Settings");
        ui.label(RichText::new("Changes are saved right away.").size(TXT_SMALL).color(FAINT()));
        ui.add_space(8.0);
        let before = (self.prefs.open_inline, self.prefs.auto_save, self.prefs.always_writable, self.prefs.confirm_save, self.prefs.default_cols, self.prefs.timeout_ms, self.prefs.shell_runtime.clone(), self.prefs.mongodump.clone(), self.prefs.mongorestore.clone(), self.prefs.auto_update, self.prefs.show_fps, self.prefs.object_preview);
        // The sections scroll inside the dialog, which keeps its title and Done in the window.
        let max_h = (ui.ctx().content_rect().height() - 190.0).max(200.0);
        egui::ScrollArea::vertical().id_salt("settings-scroll").max_height(max_h).min_scrolled_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            settings_section(ui, "APPEARANCE", |ui| {
                setting_row(ui, "Theme", "System follows the macOS light or dark setting.", |ui| {
                    let was = self.prefs.appearance;
                    segmented(ui, &mut self.prefs.appearance, &[(Appearance::System, "System"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")]);
                    if self.prefs.appearance != was {
                        self.prefs.appearance.apply(ui.ctx());
                        self.save_prefs();
                    }
                });
            });
            settings_section(ui, "DOCUMENTS", |ui| {
                setting_row(ui, "Open documents", "Where a clicked row shows its document.", |ui| {
                    segmented(ui, &mut self.prefs.open_inline, &[(false, "Side panel"), (true, "Under the row")]);
                });
                ui.separator();
                setting_row(ui, "Columns shown", "How many fields the table shows before you pick your own.", |ui| {
                    stepper(ui, &mut self.prefs.default_cols, 3, 30);
                });
                ui.separator();
                setting_row(ui, "Big objects in columns", "Objects and arrays too long for a cell show their size, or their first characters as in Mingo. Small ones always show in full.", |ui| {
                    segmented(ui, &mut self.prefs.object_preview, &[(false, "Size"), (true, "First characters")]);
                });
            });
            settings_section(ui, "EDITING", |ui| {
                setting_row(ui, "Auto-save edits", "Write each edit right away and offer Undo for 8 seconds.", |ui| {
                    toggle(ui, &mut self.prefs.auto_save);
                });
                ui.separator();
                setting_row(ui, "Always allow writes", "Skip read-only mode on every connection and save edits without showing the changes first.", |ui| {
                    toggle(ui, &mut self.prefs.always_writable);
                });
                ui.separator();
                let auto = self.prefs.auto_save || self.prefs.always_writable;
                setting_row(ui, "Show changes before saving", "List every changed field before writing unsaved edits.", |ui| {
                    ui.add_enabled_ui(!auto, |ui| toggle(ui, &mut self.prefs.confirm_save));
                });
                ui.label(RichText::new(if self.prefs.always_writable { "A save never overwrites someone else's change." } else { "Writes always need the connection unlocked, and a save never overwrites someone else's change." }).size(TXT_SMALL).color(FAINT()));
            });
            settings_section(ui, "QUERIES", |ui| {
                setting_row(ui, "Query timeout", "How long a query may run before it is stopped.", |ui| {
                    egui::ComboBox::from_id_salt("timeout").width(90.0).selected_text(format!("{} s", self.prefs.timeout_ms / 1000)).show_ui(ui, |ui| {
                        // Same presets as the original's preferences.
                        for s in [5, 10, 30, 60, 300, 3600] {
                            ui.selectable_value(&mut self.prefs.timeout_ms, s * 1000, format!("{s} s"));
                        }
                    });
                });
            });
            settings_section(ui, "SHELL", |ui| {
                let detected = find_bun("").map(|p| p.display().to_string());
                let desc = match &detected {
                    Some(p) => format!("Found automatically at {p}. Set a path only to use another Bun."),
                    None => "Bun was not found. Install it from bun.sh or enter its path.".to_string(),
                };
                setting_row(ui, "Bun runtime", &desc, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.prefs.shell_runtime).hint_text("automatic").desired_width(190.0));
                });
            });
            settings_section(ui, "PERFORMANCE", |ui| {
                setting_row(ui, "Show frame rate", "Frames per second and the time each frame takes, in the status bar. Quarry follows your display's refresh rate (vsync), up to 120 Hz on ProMotion screens.", |ui| {
                    toggle(ui, &mut self.prefs.show_fps);
                });
            });
            settings_section(ui, "UPDATES", |ui| {
                let desc = if self.update_note.is_empty() { format!("Version {}. New releases of {} are installed in the background, then Quarry restarts when idle with nothing unsaved.", env!("CARGO_PKG_VERSION"), crate::update::REPO) } else { format!("Version {}. {}", env!("CARGO_PKG_VERSION"), self.update_note) };
                setting_row(ui, "Update automatically", &desc, |ui| {
                    toggle(ui, &mut self.prefs.auto_update);
                });
                if !self.demo && !cfg!(debug_assertions) && self.updated.is_none() && ui.button("Check now").clicked() {
                    self.check_update();
                }
            });
            settings_section(ui, "DATABASE TOOLS", |ui| {
                for (i, (name, path)) in [("mongodump", &mut self.prefs.mongodump), ("mongorestore", &mut self.prefs.mongorestore)].into_iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    let desc = match find_exe(name, "") {
                        Some(p) => format!("Found automatically at {}.", p.display()),
                        None => "Not found. Install the MongoDB Database Tools or enter its path.".to_string(),
                    };
                    setting_row(ui, name, &desc, |ui| {
                        ui.add(egui::TextEdit::singleline(path).hint_text("automatic").desired_width(190.0));
                    });
                }
            });
            if find_exe("mongodump", "").is_none() || find_exe("mongorestore", "").is_none() {
                self.install_tools_button(ui);
            }
        });
        if done(ui) {
            *keep = false;
        }
        // The pref unlocks every connection each frame, so turning it off has to lock them back.
        if before.2 && !self.prefs.always_writable {
            self.conns.iter_mut().for_each(|c| c.writable = false);
        }
        if before != (self.prefs.open_inline, self.prefs.auto_save, self.prefs.always_writable, self.prefs.confirm_save, self.prefs.default_cols, self.prefs.timeout_ms, self.prefs.shell_runtime.clone(), self.prefs.mongodump.clone(), self.prefs.mongorestore.clone(), self.prefs.auto_update, self.prefs.show_fps, self.prefs.object_preview) {
            self.save_prefs();
        }
    }

    fn relations_dialog(&mut self, ui: &mut egui::Ui, conn: &mut u64, db: &mut String, keep: &mut bool) {
        ui.set_max_width(860.0);
        ui.set_width(820.0);
        let colls: Vec<String> = self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.clone()).unwrap_or_default();
        dialog_title(ui, format!("Data relations in {db}"));
        ui.label(RichText::new("A field that holds ids of another collection's documents. Its preview field is shown next to the ids, and \"Go to referenced document\" opens them. Add them by analyzing, or by right-clicking an id and choosing that action.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(6.0);
        let key = format!("{conn}/{db}");
        let before = self.prefs.relations.get(&key).cloned().unwrap_or_default();
        let list = self.prefs.relations.entry(key.clone()).or_default();
        if list.is_empty() {
            ui.label(RichText::new("None yet.").color(FAINT()));
        }
        let mut remove = None;
        egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("rels").num_columns(4).striped(true).spacing([12.0, 8.0]).min_col_width(60.0).show(ui, |ui| {
                caption(ui, "FIELD");
                caption(ui, "POINTS TO COLLECTION");
                caption(ui, "PREVIEW FIELD");
                ui.end_row();
                for (i, r) in list.iter_mut().enumerate() {
                    let name = format!("{}.{}", r.coll, r.path);
                    ui.allocate_ui_with_layout(egui::vec2(260.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_width(260.0);
                        ui.add(egui::Label::new(RichText::new(&name).monospace().color(KEY())).truncate()).on_hover_text(&name);
                    });
                    egui::ComboBox::from_id_salt(("rel-target", i)).width(250.0).selected_text(&r.target).show_ui(ui, |ui| {
                        for c in &colls {
                            ui.selectable_value(&mut r.target, c.clone(), c);
                        }
                    });
                    ui.add_sized([180.0, BUTTON_H], egui::TextEdit::singleline(&mut r.preview).hint_text("_id"));
                    if danger(ui, "Remove").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(i) = remove {
            list.remove(i);
        }
        if *list != before {
            self.save_prefs();
        }
        ui.add_space(6.0);
        let mut analyze = None;
        ui.horizontal(|ui| {
            if ui.button("Analyze all collections").on_hover_text("Samples the 10 newest documents of each collection and looks their ids up in the others").clicked() {
                analyze = Some(String::new());
            }
            let cur = self.t().filter(|t| t.sel.conn == *conn && t.sel.db == *db).map(|t| t.sel.coll.clone());
            if let Some(c) = cur {
                if ui.button(format!("Analyze {c}")).clicked() {
                    analyze = Some(c);
                }
            }
        });
        let mut note: Option<db::Res<String>> = None;
        let mut changed = false;
        ui.horizontal(|ui| {
            let list = self.prefs.relations.entry(key.clone()).or_default();
            if ui.add_enabled(!list.is_empty(), egui::Button::new("Export…")).clicked() {
                if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{db}-data-relations.json")).save_file() {
                    match std::fs::write(&path, relations_export(db, list)) {
                        Ok(()) => note = Some(Ok(format!("Exported {} data relations", list.len()))),
                        Err(e) => note = Some(Err(format!("Failed to export relations: {e}"))),
                    }
                }
            }
            if ui.button("Import…").on_hover_text("A file exported here or from Mingo; relations already present are skipped").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                    match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| relations_import(&t)) {
                        Ok(new) => {
                            let mut added = 0;
                            for r in new {
                                if !list.iter().any(|x| x.coll == r.coll && x.path == r.path && x.target == r.target) {
                                    list.push(r);
                                    added += 1;
                                }
                            }
                            changed = true;
                            note = Some(Ok(format!("Imported {added} data relations")));
                        }
                        Err(e) => note = Some(Err(format!("Failed to import relations: {e}"))),
                    }
                }
            }
            let n = self.prefs.relations.get(&key).map_or(0, Vec::len);
            ui.add_enabled_ui(n > 0, |ui| {
                ui.menu_button(RichText::new("Remove all…").color(ALERT()), |ui| {
                    if danger(ui, format!("Remove all {n} relations of {db}")).clicked() {
                        self.prefs.relations.remove(&key);
                        changed = true;
                        ui.close();
                    }
                });
            });
        });
        if changed {
            self.save_prefs();
        }
        match note {
            Some(Ok(m)) => self.ok(m),
            Some(Err(e)) => self.err(e),
            None => {}
        }
        if let Some(c) = analyze {
            match self.conn(*conn).and_then(|x| x.client.clone()) {
                Some(client) => self.analyze_relations(*conn, client, db.clone(), c),
                None => self.err("Connect first."),
            }
        }
        if done(ui) {
            *keep = false;
            let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == *conn && t.sel.db == *db).map(|t| t.req).collect();
            for r in reqs {
                if let Some(t) = self.tab_by_req(r) {
                    t.related.clear();
                }
                self.load_related(r);
            }
        }
    }
}
