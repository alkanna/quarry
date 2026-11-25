//! Applies each `db::Msg` from the worker to the app state.

use super::*;

impl App {
    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        self.view_requests();
        while let Ok(m) = self.core.rx.try_recv() {
            if Core::handles(&m) {
                // An agent call depends on what the window shows right now: focus, typing and an open dialog.
                if matches!(m, Msg::Agent(_)) {
                    self.core.agents.ui = quarry_core::state::agents::AgentUi { focused: ctx.input(|i| i.viewport().focused.unwrap_or(true)), typing: ctx.text_edit_focused(), modal: self.modal.is_some() };
                }
                self.core.on_msg(m);
                continue;
            }
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
                    if let Some((_, sel, index, value)) = self.core.dup_wait.take_if(|w| w.0 == req) {
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
                Msg::SqlJob { req, res } => self.sql_job_done(req, res),
                Msg::SqlLists { conn, schema, res } => self.sql_lists_done(conn, schema, res),
                Msg::SqlTableInfo { req, res } => self.import_info(req, res),
                Msg::SqlSchema { conn, database, schema, res } => {
                    // A failed read leaves the database without a schema until the next Refresh; typing still works.
                    if let Ok(s) = res {
                        let key = if database.is_empty() { schema } else { ns_key(&database, &schema) };
                        self.put_sql_schema(conn, key, Arc::new(s));
                    }
                }
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
                Msg::SqlRan { .. } | Msg::SqlCount { .. } | Msg::SqlLint { .. } => {}
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
                Msg::DbChanged { conn, db, copied, dropped, res } => self.on_db_changed(conn, db, copied, dropped, res),
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
                Msg::FieldValues { key, path, prefix, res } => self.values_done(key, path, prefix, res),
                Msg::Notice { title, body } => self.show_late(Modal::Text { title, body, code: false }),
                Msg::CloudSql { req, found } => self.cloud_found(req, found),
                // Link 0 is the Link a folder dialog's preview, which fills a form the app's Modal holds.
                Msg::LibScan { scan, .. } => {
                    if let Some(Modal::LinkFolder(f)) = &mut self.modal {
                        f.got_preview(scan);
                    }
                }
                Msg::JsLs { req, out } => self.js_answer(ctx, req, out),
                Msg::JsLsStopped { failed } => self.js_stopped(failed),
                // Core::handles took the Shell, Watch and database tools answers above.
                Msg::ShellLog { .. } | Msg::ShellQuery { .. } | Msg::ShellDone { .. } | Msg::WatchEvent { .. } | Msg::WatchDone { .. } | Msg::ToolLog(_) | Msg::ToolDone(_) | Msg::ToolsInstalled(_) => {}
                // Core::handles took the Compare, Schema, relations and field menu answers above.
                Msg::Compared { .. } | Msg::Schema { .. } | Msg::Analyzed { .. } | Msg::Grouped { .. } | Msg::ColumnValues { .. } | Msg::Related { .. } | Msg::RefFound { .. } => {}
            }
        }
    }

    fn on_command(&mut self, tag: String, res: db::Res<Document>) {
        match (tag.as_str(), res) {
            (t, Ok(d)) if t.starts_with("tabstats\u{1f}") => {
                let key: Vec<&str> = t.split('\u{1f}').skip(1).collect();
                let n = |k: &str| match d.get(k) {
                    Some(Bson::Int32(n)) => *n as u64,
                    Some(Bson::Int64(n)) => *n as u64,
                    Some(Bson::Double(n)) => *n as u64,
                    _ => 0,
                };
                for tab in self.core.tabs.iter_mut().filter(|tab| [tab.sel.conn.to_string().as_str(), &tab.sel.db, &tab.sel.coll] == key[..]) {
                    tab.stats = Some((n("count"), n("size"), n("nindexes")));
                }
            }
            ("shpg", Ok(d)) => self.shell_pg_listed(d),
            // Nice to have: a server that refuses these just shows less.
            (t, Err(_)) if t.starts_with("tabstats\u{1f}") => {}
            ("stats", Ok(d)) => {
                let title = format!("Statistics of {}", d.get_str("collection").unwrap_or_default());
                let mut lines = Vec::new();
                for (k, v) in d.iter().filter(|(k, _)| *k != "collection") {
                    let n = match v {
                        Bson::Int32(n) => Some(*n as f64),
                        Bson::Int64(n) => Some(*n as f64),
                        Bson::Double(n) => Some(*n),
                        _ => None,
                    };
                    let text = match (k.as_str(), n) {
                        ("size" | "avgObjSize" | "storageSize" | "totalIndexSize", Some(n)) => human_bytes(n as usize),
                        (_, Some(n)) => group(&(n as i64).to_string()),
                        ("indexSizes", _) => v.as_document().map(|d| d.iter().map(|(i, s)| format!("{i}: {}", human_bytes(match s { Bson::Int32(n) => *n as usize, Bson::Int64(n) => *n as usize, Bson::Double(n) => *n as usize, _ => 0 }))).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                        _ => db::value_text(v),
                    };
                    lines.push(format!("{k}: {text}"));
                }
                self.modal = Some(Modal::Text { title, body: lines.join("\n"), code: false });
            }
            ("viewdef", Ok(d)) => {
                let opts = d.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).and_then(|a| a.first()).and_then(|x| x.as_document()).and_then(|x| x.get_document("options").ok()).cloned().unwrap_or_default();
                let body = format!("viewOn: {}\npipeline:\n{}", opts.get_str("viewOn").unwrap_or_default(), db::to_edit_value(opts.get("pipeline").unwrap_or(&Bson::Array(vec![]))));
                self.modal = Some(Modal::Text { title: "View definition".into(), body, code: true });
            }
            (tag, Ok(d)) if tag.starts_with("validator:") => self.core.validator_loaded(tag["validator:".len()..].parse().unwrap_or(0), d),
            (_, Err(e)) => self.err(e),
            _ => {}
        }
    }

    fn on_db_changed(&mut self, conn: u64, db: String, copied: Option<String>, dropped: Option<String>, res: db::Res<String>) {
        if let Some(to) = copied {
            // Listed now so the row does not blink out before the reload below answers.
            if res.is_ok() && let Some(l) = self.core.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == db)).and_then(|d| d.1.as_mut()) {
                l.push(to.clone());
                l.sort();
            }
            self.core.copying.remove(&(conn, db.clone(), to));
        }
        let open = self.core.open_after.take_if(|o| o.0 == conn && o.1 == db);
        if res.is_ok() && open.is_some() && let Some(c) = self.core.conns.iter_mut().find(|c| c.meta.id == conn) && !c.dbs.iter().any(|d| d.0 == db) {
            // A new database is listed now so its collection has a row to open under.
            c.dbs.push((db.clone(), None));
            c.dbs.sort_by(|a, b| a.0.cmp(&b.0));
        }
        if res.is_ok() && let Some((_, _, coll)) = open {
            self.open_collection(Sel { conn, db: db.clone(), coll, ..Default::default() }, Open::Here);
        }
        match res {
            Ok(m) => {
                // Only a drop the server confirmed forgets its undo entries; a refused one keeps them.
                if let Some(coll) = dropped {
                    self.purge_mongo_undo(conn, &db, &coll);
                }
                self.ok(m)
            }
            Err(e) => {
                self.err(e);
                if dropped.is_some_and(|c| c.is_empty()) && let Some(c) = self.core.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.dbs.push((db.clone(), None));
                    c.dbs.sort_by(|a, b| a.0.cmp(&b.0));
                }
            }
        }
        // The server may normalise the validator, so show what it kept.
        for t in self.core.tabs.iter_mut().filter(|t| t.sel.conn == conn && t.sel.db == db) {
            t.schema.vloaded = false;
        }
        if let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) {
            if db == "*" {
                self.connect(conn);
            } else {
                self.core.worker.collections(conn, client, db);
            }
        }
        if self.core.users.as_ref().is_some_and(|u| u.conn == conn) {
            self.core.load_users(conn);
        }
        // Hiding an index or switching the profiler changes what the Indexes view shows.
        if self.t().is_some_and(|t| t.view == View::Indexes) {
            self.load_indexes();
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
