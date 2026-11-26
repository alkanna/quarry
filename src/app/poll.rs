//! Applies each `db::Msg` from the worker to the app state.

use super::*;

impl App {
    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        // An agent call depends on what the window shows right now: focus, typing and an open dialog.
        self.core.set_agent_ui(quarry_core::state::agents::AgentUi { focused: ctx.input(|i| i.viewport().focused.unwrap_or(true)), typing: ctx.text_edit_focused(), modal: self.modal.is_some() });
        for m in self.core.tick() {
            match m {
                Msg::SqlOrphans { req, res, sample } => {
                    if let Some(Modal::FkEdit(f)) = self.modal.as_mut() {
                        f.orphans_done(req, res, sample);
                    }
                }
                Msg::SqlFkRules { req, strict } => {
                    if let Some(Modal::FkEdit(f)) = self.modal.as_mut() {
                        f.rules_done(req, strict);
                    }
                }
                Msg::SqlStructure { req, res } => {
                    if let Some((_, sel, index, value)) = self.core.take_dup_wait(req) {
                        self.sql_dup_index(sel, &index, value, res);
                        continue;
                    }
                    // The Add relation dialog asks for the table it points at on a request of its own.
                    if let Some(Modal::FkEdit(f)) = self.modal.as_mut() && f.parent_loaded(req, res.clone()) {
                        continue;
                    }
                    // So does the New table dialog, for the parent of each of its foreign keys. MySQL and PostgreSQL each have their own.
                    if let Some(keys) = self.modal.as_mut().and_then(Modal::new_keys_mut) && keys.parent_loaded(req, &res) {
                        continue;
                    }
                    self.core.structure_loaded(req, res);
                }
                // The Explain dialog's state is in the Modal; Core works the answers out on it.
                Msg::SqlExplained { req, sets } => self.sql_explained(req, sets),
                Msg::SqlExplainEvidence { req, ev } => self.sql_explain_evidence(req, ev),
                Msg::SqlExplainHypo { req, res } => self.sql_explain_hypo(req, res),
                Msg::SqlBulkPrep { req, res } => self.on_bulk_prep(req, res),
                Msg::SqlExport { req, res } => self.export_done(ctx, req, res),
                Msg::SqlTableInfo { req, res } => self.import_info(req, res),
                Msg::SqlMeta { conn, schema, res } => self.sql_meta_done(conn, schema, res),
                Msg::SqlViewDef { conn, database, schema, name, res } => self.sql_view_def_done(conn, database, schema, name, res),
                Msg::SqlViewSaved { conn, database, schema, name, kind, res, undo, need_drop, sql_mode } => self.sql_view_saved(conn, database, schema, name, kind, res, undo, need_drop, sql_mode),
                Msg::PgRoles { conn, req, ans: db::sql::pg::roles::Answer::DropInfo(res) } => {
                    if let Some(Modal::PgDropRole(f)) = &mut self.modal && f.conn == conn && f.req == req {
                        f.info = Some(res);
                    }
                }
                // Core::handles took these above.
                Msg::SqlUsers { .. } | Msg::SqlUserGrants { .. } | Msg::SqlUserDone { .. } | Msg::PgRoles { .. } => {}
                Msg::SqlServer { .. } | Msg::PgServer { .. } => {}
                Msg::Connected { .. } | Msg::SqlConnected { .. } | Msg::Tested { .. } | Msg::Collections { .. } | Msg::SqlSchemas { .. } | Msg::SqlTables { .. } => {}
                Msg::UpdateChecked(_) | Msg::UpdateInstalled(_) | Msg::LibFile { .. } => {}
                Msg::SqlRan { .. } | Msg::SqlCount { .. } | Msg::SqlLint { .. } | Msg::SqlSchema { .. } | Msg::SqlJob { .. } | Msg::SqlLists { .. } => {}
                Msg::SqlCols { .. } | Msg::SqlFilterCount { .. } | Msg::SqlKeys { .. } | Msg::SqlValues { .. } | Msg::SqlFacts { .. } => {}
                Msg::SqlAltered { .. } => {}
                Msg::SqlRoutine { .. } => {}
                Msg::Agent(_) => {}
                Msg::SqlDropInfo { conn, database, schema, table, res } => {
                    self.truncate_info_done(conn, &database, &schema, &table, &res);
                    self.sql_drop_info_done(conn, database, schema, table, res)
                }
                Msg::PgDbChoices { conn, res } => self.pg_choices_done(conn, res),
                Msg::SqlSizes { req, res } => self.sql_sizes_done(req, res),
                Msg::Command { tag, res } => self.on_command(tag, res),
                // Core::handles took these above.
                Msg::DbChanged { .. } | Msg::FieldValues { .. } => {}
                Msg::Docs { req, res, approx } => self.core.docs_loaded(req, res, approx, &page_info),
                // Core::handles took these above.
                Msg::IndexInfo { .. } | Msg::Indexes { .. } | Msg::Written { .. } | Msg::Saved { .. } | Msg::Aggregated { .. } | Msg::StagePreview { .. } => {}
                Msg::SqlDdl { .. } | Msg::SqlRenamed { .. } => {}
                Msg::Explained { req, res } if req == self.core.explain_req => match res {
                    Ok(raw) => {
                        self.show_late(Modal::Explain { summary: db::explain_summary(&raw), raw: db::to_edit_json(&raw), show_raw: false });
                    }
                    Err(e) => self.err(e),
                },
                Msg::Explained { .. } => {}
                Msg::UpdatePreview { stages, res } => {
                    if let Some(Modal::UpdateMany { preview, .. }) = &mut self.modal {
                        *preview = Some((stages, res));
                    }
                }
                Msg::Info(res) => match res {
                    Ok(s) => self.ok(s),
                    Err(e) => self.err(e),
                }
                Msg::Notice { title, body } => self.show_late(Modal::Text { title, body, code: false }),
                // Link 0 is the Link a folder dialog's preview, which fills a form the app's Modal holds.
                Msg::LibScan { scan, .. } => {
                    if let Some(Modal::LinkFolder(f)) = &mut self.modal {
                        f.got_preview(scan);
                    }
                }
                Msg::JsLs { req, out } => self.js_answer(ctx, req, out),
                Msg::JsLsStopped { failed } => self.js_stopped(failed),
                // Core::handles took the Shell, Watch and database tools answers above.
                Msg::ShellLog { .. } | Msg::ShellQuery { .. } | Msg::ShellDone { .. } | Msg::WatchEvent { .. } | Msg::WatchDone { .. } | Msg::ToolLog(_) | Msg::ToolDone(_) | Msg::ToolsInstalled(_) | Msg::CloudSql { .. } => {}
                // Core::handles took the Compare, Schema, relations and field menu answers above.
                Msg::Compared { .. } | Msg::Schema { .. } | Msg::Analyzed { .. } | Msg::Grouped { .. } | Msg::ColumnValues { .. } | Msg::Related { .. } | Msg::RefFound { .. } => {}
            }
        }
    }

    fn on_command(&mut self, tag: String, res: db::Res<Document>) {
        match (tag.as_str(), res) {
            ("shpg", Ok(d)) => self.shell_pg_listed(d),
            // Nice to have: a server that refuses these just shows less.
            (t, Err(_)) if t.starts_with("tabstats\u{1f}") => {}
            (_, Err(e)) => self.err(e),
            _ => {}
        }
    }

    /// An answer that opens a dialog waits for the one already open, which may hold the user's typing.
    fn show_late(&mut self, m: Modal) {
        if self.modal.is_some() {
            self.late_modals.push(m);
        } else {
            self.modal = Some(m);
        }
    }
}

impl App {
    pub(super) fn page_size(&self) -> u64 {
        self.core.page_size()
    }
}

/// The field paths and suggestions of a page, which the autocomplete builds: the rows, then the values fetched before.
fn page_info(docs: &[db::Row], values: Option<&HashMap<String, Vec<Bson>>>) -> (Vec<String>, Fields) {
    let mut f = field_info(docs);
    if let Some(v) = values {
        merge_values(&mut f, v);
    }
    (doc_paths(docs), f)
}
