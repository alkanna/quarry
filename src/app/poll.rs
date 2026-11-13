//! Applies each `db::Msg` from the worker to the app state.

use super::*;

impl App {
    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Connected { conn, attempt, res } => self.on_connected(conn, attempt, res),
                Msg::Agent(ev) => self.on_agent(ctx, ev),
                Msg::SqlConnected { conn, attempt, res } => self.on_sql_connected(conn, attempt, res),
                Msg::SqlCols { req, cols, pk, estimated, matched } => {
                    if let Some(s) = self.tab_by_req(req).and_then(|t| t.sql.as_mut()) {
                        // The rules pick from table columns, which a grouped answer does not have.
                        let cols = Arc::new(cols);
                        if matched.is_none() {
                            s.bar.table_cols = cols.clone();
                        }
                        (s.cols, s.pk, s.estimated, s.matched) = (cols, pk, estimated, matched);
                    }
                }
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
                    // The Add relation dialog asks for the table it points at on a request of its own.
                    if let Some(Modal::FkEdit(f)) = self.modal.as_mut() && f.parent_loaded(req, res.clone()) {
                        continue;
                    }
                    // So does the New table dialog, for the parent of each of its foreign keys.
                    if let Some(Modal::PgNewTable(f)) = self.modal.as_mut() && f.parent_loaded(req, &res) {
                        continue;
                    }
                    if let Some(s) = self.tabs.iter_mut().find(|t| t.idx_req == req).and_then(|t| t.sql.as_mut()) {
                        s.st.finish(res);
                    }
                }
                Msg::SqlAltered { conn, database, schema, table, res, undo, sql_mode, alter, unsure } => self.sql_altered(conn, database, schema, table, res, undo, sql_mode, alter, unsure),
                Msg::SqlRan { req, sets, .. } => self.sql_ran(req, sets),
                Msg::SqlCount { req, idx, res } => self.sql_counted(req, idx, res),
                Msg::SqlExport { req, res } => self.export_done(ctx, req, res),
                Msg::SqlJob { req, res } => self.sql_job_done(req, res),
                Msg::SqlServer { req, ans } => self.sql_server_done(req, ans),
                Msg::PgServer { req, ans } => self.pg_server_done(req, ans),
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
                Msg::SqlUsers { conn, req, res } => self.sql_users_loaded(conn, req, res),
                Msg::SqlUserGrants { conn, req, res } => self.sql_user_grants_loaded(conn, req, res),
                Msg::SqlUserDone { conn, account, op, res, undo } => self.sql_user_done(conn, account, op, res, undo),
                Msg::PgRoles { conn, req, ans } => self.pg_roles_answer(conn, req, ans),
                Msg::SqlDropInfo { conn, database, schema, table, res } => self.sql_drop_info_done(conn, database, schema, table, res),
                Msg::PgDbChoices { conn, res } => self.pg_choices_done(conn, res),
                Msg::SqlDdl { conn, database, schema, table, what, res, undo, dbs, sql_mode } => self.sql_ddl_done(conn, database, schema, table, what, res, undo, dbs, sql_mode),
                Msg::SqlKeys { req, res } => self.sql_keys_done(req, res),
                Msg::SqlValues { conn, database, schema, table, column, prefix, res } => self.sql_values_done(conn, database, schema, table, column, prefix, res),
                Msg::SqlSchemas { conn, database, res } => self.pg_schemas_done(conn, database, res),
                Msg::SqlTables { conn, database, schema, res } => self.pg_tables_done(conn, database, schema, res),
                Msg::SqlSizes { req, res } => self.sql_sizes_done(req, res),
                Msg::Collections { conn, db, res } => match res {
                    Ok((v, views)) => {
                        self.side_err.remove(&(conn, db.clone()));
                        self.views.set(conn, &db, views);
                        if let Some(d) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == db)) {
                            d.1 = Some(v);
                        }
                    }
                    Err(e) => {
                        self.side_err.insert((conn, db.clone()), e.clone());
                        self.err_quiet(e, Some(conn), Some(Sel { conn, db: db.clone(), ..Default::default() }), format!("List collections · {db}"))
                    }
                },
                Msg::Command { tag, res } => self.on_command(tag, res),
                Msg::DbChanged { conn, db, copied, res } => self.on_db_changed(conn, db, copied, res),
                Msg::WatchEvent { req, doc } => self.watch_event(req, doc),
                Msg::WatchDone { req, res } => self.watch_done(req, res),
                Msg::IndexInfo { key, res } => match res {
                    Ok(i) => {
                        self.idx_info = Some((key.clone(), i));
                        self.refresh_idx_rows(&key);
                    }
                    Err(e) => self.err(e),
                },
                Msg::Docs { req, res } => self.on_docs(req, res),
                Msg::Indexes { req, res } => match (res, self.tabs.iter().position(|t| t.idx_req == req)) {
                    (Ok(v), Some(i)) => {
                        let t = &mut self.tabs[i];
                        t.clear_failed(View::Indexes);
                        t.set_indexes(v);
                        let key = t.sel.key();
                        self.refresh_idx_rows(&key);
                    }
                    (Err(e), Some(i)) => self.fail_view(i, View::Indexes, e, "list the indexes"),
                    _ => {}
                },
                Msg::Explained { req, res } if req == self.explain_req => match res {
                    Ok(raw) => self.modal = Some(Modal::Explain { summary: db::explain_summary(&raw), raw: db::to_edit_json(&raw), show_raw: false }),
                    Err(e) => self.err(e),
                },
                Msg::Explained { .. } => {}
                Msg::UpdatePreview { stages, res } => {
                    if let Some(Modal::UpdateMany { preview, .. }) = &mut self.modal {
                        *preview = Some((stages, res));
                    }
                }
                Msg::StagePreview { req, res } => {
                    let Some(st) = self.tabs.iter_mut().flat_map(|t| std::iter::once(&mut t.agg_input).chain(t.stages.iter_mut())).find(|s| s.req == req) else { continue };
                    st.req = 0;
                    match res {
                        Ok((n, docs, ms)) => {
                            // Rows stay open when a refresh brings back the same documents.
                            if st.sample != docs {
                                st.open.clear();
                            }
                            (st.count, st.sample, st.ms, st.error) = (n, docs, Some(ms), None);
                        }
                        Err(e) => (st.count, st.sample, st.ms, st.error) = (None, Vec::new(), None, Some(e)),
                    }
                }
                Msg::Aggregated { req, res } => self.on_aggregated(req, res),
                Msg::Written { at, res, undo, reload, retry } => self.on_written(at, res, undo, reload, retry),
                Msg::Saved { tag, res, undo } => self.on_saved(tag, res, undo),
                Msg::Info(res) => match res {
                    Ok(s) => self.ok(s),
                    Err(e) => self.err(e),
                }
                Msg::ColumnValues { conn, op, coll, path, res } => self.column_values_done(ctx, conn, op, coll, path, res),
                Msg::FieldValues { key, path, prefix, res } => self.values_done(key, path, prefix, res),
                Msg::Notice { title, body } => self.modal = Some(Modal::Text { title, body, code: false }),
                Msg::Grouped { conn, db, coll, path, pipeline, res } => match res {
                    Ok(rows) => {
                        self.ok(format!("{} distinct values of {path}", rows.len()));
                        self.modal = Some(Modal::Grouped { sel: Sel { conn, db, coll, ..Default::default() }, path, pipeline, rows });
                    }
                    Err(e) => self.err(e),
                },
                Msg::Related { req, res } => match (res, self.tab_by_req(req)) {
                    (Ok(v), Some(t)) => t.related.extend(v),
                    (Err(e), _) => self.err(format!("Could not load related documents: {e}")),
                    _ => {}
                },
                Msg::RefFound { conn, db, coll, path, value, res } => match res {
                    Ok(Some((target, preview))) => {
                        self.prefs.relations.entry(format!("{conn}/{db}")).or_default().push(Relation { coll: coll.clone(), path: path.clone(), target: target.clone(), preview: preview.clone() });
                        self.save_prefs();
                        let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == db && t.sel.coll == coll).map(|t| t.req).collect();
                        for r in reqs {
                            self.load_related(r);
                        }
                        self.go_ref(conn, db, target.clone(), value);
                        self.ok(format!("Linked {path} to {target}, showing {preview} next to its ids. Edit it under Data relations in the database menu."));
                    }
                    Ok(None) => self.err(format!("No collection of {db} has a document with this _id.")),
                    Err(e) => self.err(e),
                },
                Msg::Analyzed { conn, db, res } => match res {
                    Ok(found) => {
                        let list = self.prefs.relations.entry(format!("{conn}/{db}")).or_default();
                        let mut added = 0;
                        for (coll, path, target, preview) in found {
                            if !list.iter().any(|r| r.coll == coll && r.path == path) {
                                list.push(Relation { coll, path, target, preview });
                                added += 1;
                            }
                        }
                        self.save_prefs();
                        let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == db).map(|t| t.req).collect();
                        for r in reqs {
                            self.load_related(r);
                        }
                        self.ok(if added == 0 { "Analysis done: no new relations found.".to_string() } else { format!("Analysis done: {added} new relation{} found.", if added == 1 { "" } else { "s" }) });
                        if self.modal.is_none() {
                            self.modal = Some(Modal::Relations { conn, db });
                        }
                    }
                    Err(e) => self.err(format!("Could not analyze relations: {e}")),
                },
                Msg::Compared { req, res } => {
                    let Some(i) = self.tabs.iter().position(|t| t.compare.req == req) else { continue };
                    let t = &mut self.tabs[i];
                    t.compare.running = false;
                    match res {
                        Ok(r) => {
                            t.clear_failed(View::Compare);
                            t.compare.set_result(Some(r));
                        }
                        Err(e) => self.fail_view(i, View::Compare, e, "compare"),
                    }
                }
                Msg::ToolLog(line) => {
                    if let Some(t) = &mut self.tool {
                        t.log.push(line);
                        if t.log.len() > 2000 {
                            t.log.drain(..500);
                        }
                    }
                }
                Msg::Update(res) => match res {
                    Ok(Some(v)) => {
                        self.update_note = format!("Version {v} is installed and starts on restart.");
                        self.updated = Some(v);
                    }
                    Ok(None) => self.update_note = format!("Up to date (version {}).", env!("CARGO_PKG_VERSION")),
                    // Offline or rate-limited checks stay quiet; the reason shows in Settings.
                    Err(e) => self.update_note = format!("Last check failed: {e}"),
                },
                Msg::ToolsInstalled(res) => {
                    // Only a click shows failures; a background update that fails stays quiet, like the app's own.
                    let manual = std::mem::take(&mut self.tools_installing);
                    match res {
                        Ok(Some(v)) => self.ok(format!("{} the MongoDB Database Tools {v} in {}", if manual { "Installed" } else { "Updated" }, tools_dir().display())),
                        Ok(None) => {}
                        Err(e) if manual => self.err(format!("Could not install the MongoDB Database Tools: {e}")),
                        Err(_) => {}
                    }
                }
                Msg::CloudSql { req, found } => self.cloud_found(req, found),
                Msg::LibScan { link, scan } => self.apply_scan(link, scan),
                Msg::LibFile { job, out } => self.file_done(job, out),
                Msg::ToolDone(res) => {
                    if let Some(t) = &mut self.tool {
                        t.cancel = None;
                        t.done = Some(res.clone());
                    }
                    match res {
                        Ok(m) => self.ok(m),
                        Err(e) => self.err(e),
                    }
                }
                Msg::Tested { req, res, info } => {
                    // A SQL test already says how long it took; the others get the time measured here.
                    let started = self.test_started.take();
                    let res = res.map(|m| match started {
                        Some(t) if !m.contains(" ms") => format!("{}  ·  {} ms", m.trim_end_matches('.'), t.elapsed().as_millis()),
                        _ => m,
                    });
                    self.conn_test = Some((req, res));
                    self.test_info = info.map(|i| (req, i));
                }
                Msg::Schema { req, res } => {
                    let Some(i) = self.tabs.iter().position(|t| t.schema.req == req) else { continue };
                    let t = &mut self.tabs[i];
                    t.schema.running = false;
                    match res {
                        Ok((stats, n, ms)) => {
                            t.clear_failed(View::Schema);
                            (t.schema.stats, t.schema.docs, t.schema.ms, t.schema.err) = (stats, n, Some(ms), None);
                        }
                        Err(e) => {
                            (t.schema.stats, t.schema.ms, t.schema.err) = (Vec::new(), None, Some(e.clone()));
                            self.fail_view(i, View::Schema, e, "analyze the schema");
                        }
                    }
                }
                Msg::ShellLog { req, level, text } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) {
                        t.shell.logs.push((level, text));
                    }
                }
                Msg::ShellQuery { req, q } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) {
                        // The panel keeps the latest queries only, a loop can run millions.
                        let mut q = q;
                        if q.text.len() > 2000 {
                            q.text = q.text.chars().take(2000).collect();
                        }
                        t.shell.queries.push(q);
                        // Trimmed in batches so a loop does not shift a thousand entries per query.
                        if t.shell.queries.len() > 1500 {
                            let over = t.shell.queries.len() - 1000;
                            t.shell.queries.drain(..over);
                        }
                    }
                }
                Msg::JsLs { req, out } => self.js_answer(ctx, req, out),
                Msg::JsLsStopped { failed } => self.js_stopped(failed),
                Msg::ShellDone { req, res, ms } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) else { continue };
                    t.rows_replaced();
                    let sql = t.sql.is_some();
                    let sh = &mut t.shell;
                    sh.cancel = None;
                    sh.started = None;
                    sh.ms = Some(ms);
                    let parsed = res.and_then(|ejson| {
                        let v: serde_json::Value = serde_json::from_str(&ejson).map_err(|e| e.to_string())?;
                        match Bson::try_from(v).map_err(|e| e.to_string())? {
                            Bson::Document(mut d) => Ok(d.remove("v").unwrap_or(Bson::Null)),
                            _ => Err("Unexpected result from the shell".to_string()),
                        }
                    });
                    match parsed {
                        Ok(v) => {
                            sh.set_value(v);
                            if sql {
                                sh.hide_absent_id();
                            }
                        }
                        Err(e) => {
                            sh.value = None;
                            sh.docs.clear();
                            sh.error = Some(e);
                        }
                    }
                }
            }
        }
    }

    fn on_connected(&mut self, conn: u64, attempt: u64, res: db::Res<(mongodb::Client, Vec<String>, Option<crate::ssh::Tunnel>)>) {
        // A connect the user stopped (Disconnect, removal) or replaced is dropped, and its tunnel with it.
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn && c.waits_for(attempt)) else { return };
        match res {
            Ok((client, dbs, tunnel)) => {
                // Trust the SSH host key the first time; later a different key is refused.
                let mut trusted = None;
                if let (Some(t), Some(sc)) = (&tunnel, c.meta.ssh.as_mut()) && sc.known_host.is_none() && !t.fingerprint.is_empty() {
                    sc.known_host = Some(t.fingerprint.clone());
                    trusted = Some(t.fingerprint.clone());
                }
                c.tunnel = tunnel.map(db::Way::Ssh);
                // Load every database's collections up front so the list can show counts.
                for d in &dbs {
                    self.worker.collections(conn, client.clone(), d.clone());
                }
                // Version, replica set and host for the status bar.
                self.worker.command(client.clone(), "admin".into(), doc! { "buildInfo": 1 }, &format!("srv-build:{conn}"));
                self.worker.command(client.clone(), "admin".into(), doc! { "hello": 1 }, &format!("srv-hello:{conn}"));
                c.client = Some(client);
                c.status = Status::Ready;
                c.forget_lists();
                c.dbs = dbs.into_iter().map(|d| (d, None)).collect();
                self.pg_tables_asked.forget(conn, None, None);
                let n = c.meta.name.clone();
                self.ok(format!("Connected to {n} (read-only)"));
                if let Some(fp) = trusted {
                    self.save_conns();
                    self.ok(format!("Connected to {n} through SSH (read-only). Host key remembered: {fp}"));
                }
                let here = self.cur;
                for i in 0..self.tabs.len() {
                    if self.tabs[i].sel.conn == conn && self.tabs[i].docs.is_empty() {
                        self.cur = i;
                        self.run_query();
                    }
                }
                self.cur = here;

            }
            Err(e) => {
                c.status = Status::Failed(e.clone());
                self.err_conn(conn, e);
            }
        }
    }

    fn on_sql_connected(&mut self, conn: u64, attempt: u64, res: db::Res<(db::sql::Db, Vec<String>, Option<db::Way>)>) {
        // Dropping a stopped or replaced connect's answer also stops its Cloud SQL proxy or SSH tunnel.
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn && c.waits_for(attempt)) else { return };
        match res {
            Ok((sql, dbs, tunnel)) => {
                let mut trusted = None;
                if let (Some(db::Way::Ssh(t)), Some(sc)) = (&tunnel, c.meta.ssh.as_mut()) && sc.known_host.is_none() && !t.fingerprint.is_empty() {
                    sc.known_host = Some(t.fingerprint.clone());
                    trusted = Some(t.fingerprint.clone());
                }
                c.tunnel = tunnel;
                // A server can hold hundreds of schemas, so only those on screen load now.
                self.sql_asked.retain(|k| k.0 != conn);
                let wanted: Vec<String> = dbs.iter().filter(|d| self.open_dbs.contains(&(conn, (*d).clone())) || self.tabs.iter().any(|t| t.sel.conn == conn && t.sel.db == **d)).cloned().collect();
                let pg = sql.dialect == db::sql::Dialect::Postgres;
                for d in wanted {
                    self.sql_asked.insert((conn, d.clone()));
                    // A PostgreSQL database lists its schemas first; the tables of the picked one follow.
                    if pg {
                        self.worker.sql_schemas(conn, sql.clone(), d);
                    } else {
                        self.worker.sql_tables(conn, sql.clone(), d);
                    }
                }
                c.server = ServerInfo { version: sql.label().split_once(' ').map(|(_, v)| v.to_string()).unwrap_or_default(), product: match sql.dialect { db::sql::Dialect::MySql => "MySQL", db::sql::Dialect::MariaDb => "MariaDB", db::sql::Dialect::Postgres => "PostgreSQL" }.into(), host: host_of(&c.masked_uri), tls: sql.tls, ..Default::default() };
                c.sql = Some(sql);
                c.status = Status::Ready;
                c.forget_lists();
                c.dbs = dbs.into_iter().map(|d| (d, None)).collect();
                self.pg_tables_asked.forget(conn, None, None);
                let n = c.meta.name.clone();
                self.ok(format!("Connected to {n} (read-only)"));
                if let Some(fp) = trusted {
                    self.save_conns();
                    self.ok(format!("Connected to {n} through SSH (read-only). Host key remembered: {fp}"));
                }
                for i in 0..self.tabs.len() {
                    if self.tabs[i].sel.conn == conn && self.tabs[i].docs.is_empty() {
                        self.reload_of(i);
                    }
                }
            }
            Err(e) => {
                c.status = Status::Failed(e.clone());
                self.err_conn(conn, e);
            }
        }
    }

    fn on_command(&mut self, tag: String, res: db::Res<Document>) {
        match (tag.as_str(), res) {
            (t, Ok(d)) if t.starts_with("srv-") => {
                let (kind, id) = t.split_once(':').unwrap_or_default();
                if let Some(c) = self.conns.iter_mut().find(|c| id.parse() == Ok(c.meta.id)) {
                    if kind == "srv-build" {
                        c.server.version = d.get_str("version").unwrap_or_default().to_string();
                    } else {
                        c.server.set_name = d.get_str("setName").unwrap_or_default().to_string();
                        // The member answering, else the host from the connection string.
                        c.server.host = d.get_str("me").map(str::to_string).unwrap_or_else(|_| host_of(&c.masked_uri));
                    }
                }
            }
            (t, Ok(d)) if t.starts_with("tabstats\u{1f}") => {
                let key: Vec<&str> = t.split('\u{1f}').skip(1).collect();
                let n = |k: &str| match d.get(k) {
                    Some(Bson::Int32(n)) => *n as u64,
                    Some(Bson::Int64(n)) => *n as u64,
                    Some(Bson::Double(n)) => *n as u64,
                    _ => 0,
                };
                for tab in self.tabs.iter_mut().filter(|tab| [tab.sel.conn.to_string().as_str(), &tab.sel.db, &tab.sel.coll] == key[..]) {
                    tab.stats = Some((n("count"), n("size"), n("nindexes")));
                }
            }
            ("shpg", Ok(d)) => self.shell_pg_listed(d),
            // Nice to have: a server that refuses these just shows less.
            (t, Err(_)) if t.starts_with("srv-") || t.starts_with("tabstats\u{1f}") => {}
            // Atlas and least-privilege users often may not list everyone; show who is logged in instead.
            ("users", Err(e)) if db::is_unauthorized(&e) && self.users.as_ref().is_some_and(|u| !u.self_only) => {
                let conn = self.users.as_ref().map(|u| u.conn).unwrap_or_default();
                if let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) {
                    self.worker.command(client, "admin".into(), doc! { "connectionStatus": 1 }, "users-self");
                }
                if let Some(u) = &mut self.users {
                    u.self_only = true;
                }
            }
            ("users", r) => {
                if let Some(u) = &mut self.users {
                    u.users = Some(r.map(|d| d.get_array("users").map(|a| a.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default()));
                }
            }
            ("users-self", r) => {
                if let Some(u) = &mut self.users {
                    u.users = Some(r.map(|d| self_users(&d)));
                }
            }
            ("roles", r) => {
                if let Some(u) = &mut self.users {
                    u.roles = Some(r.map(|d| d.get_array("roles").map(|a| a.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default()));
                }
            }
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
            (tag, Ok(d)) if tag.starts_with("validator:") => {
                let req: u64 = tag["validator:".len()..].parse().unwrap_or(0);
                let opts = d.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).and_then(|a| a.first()).and_then(|x| x.as_document()).and_then(|x| x.get_document("options").ok()).cloned().unwrap_or_default();
                if let Some(t) = self.tabs.iter_mut().find(|t| t.schema.vreq == req) {
                    let s = &mut t.schema;
                    s.vtext = opts.get_document("validator").map(db::to_edit_json).unwrap_or_default();
                    s.vlevel = match opts.get_str("validationLevel") {
                        Ok("off") => 0,
                        Ok("moderate") => 1,
                        _ => 2,
                    };
                    s.vwarn = opts.get_str("validationAction") == Ok("warn");
                }
            }
            (tag, r) if tag.starts_with("server:") => self.server_reply(tag, r),
            (_, Err(e)) => self.err(e),
            _ => {}
        }
    }

    fn on_db_changed(&mut self, conn: u64, db: String, copied: Option<String>, res: db::Res<String>) {
        if let Some(to) = copied {
            // Listed now so the row does not blink out before the reload below answers.
            if res.is_ok() && let Some(l) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == db)).and_then(|d| d.1.as_mut()) {
                l.push(to.clone());
                l.sort();
            }
            self.copying.remove(&(conn, db.clone(), to));
        }
        let dropped = self.dropping_db.take_if(|d| d.0 == conn && d.1 == db);
        let dropped_coll = self.dropping_coll.take_if(|d| d.0 == conn && d.1 == db);
        match res {
            Ok(m) => {
                // Only a drop the server confirmed forgets its undo entries; a refused one keeps them.
                if dropped.is_some() {
                    self.purge_mongo_undo(conn, &db, "");
                }
                if let Some((_, _, coll)) = dropped_coll {
                    self.purge_mongo_undo(conn, &db, &coll);
                }
                self.ok(m)
            }
            Err(e) => {
                self.err(e);
                if dropped.is_some() && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.dbs.push((db.clone(), None));
                    c.dbs.sort_by(|a, b| a.0.cmp(&b.0));
                }
            }
        }
        // The server may normalise the validator, so show what it kept.
        for t in self.tabs.iter_mut().filter(|t| t.sel.conn == conn && t.sel.db == db) {
            t.schema.vloaded = false;
        }
        if let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) {
            if db == "*" {
                self.connect(conn);
            } else {
                self.worker.collections(conn, client, db);
            }
        }
        if self.users.as_ref().is_some_and(|u| u.conn == conn) {
            self.load_users(conn);
        }
        // Hiding an index or switching the profiler changes what the Indexes view shows.
        if self.t().is_some_and(|t| t.view == View::Indexes) {
            self.load_indexes();
        }
    }

    fn on_docs(&mut self, req: u64, res: db::Res<(Vec<db::Row>, u64, u128)>) {
        let Some(i) = self.tabs.iter().position(|t| t.req == req) else { return };
        let t = &mut self.tabs[i];
        t.rows_replaced();
        t.loading = false;
        match res {
            Ok((docs, count, ms)) => {
                // Deleting the last page's rows leaves the tab past the end; step back to the new last page.
                let last = count.saturating_sub(1) / self.page_size();
                if docs.is_empty() && self.tabs[i].page > last {
                    self.tabs[i].page = last;
                    return self.reload_of(i);
                }
                let Self { tabs, values, .. } = self;
                let t = &mut tabs[i];
                // A query that works again clears an earlier error.
                t.clear_failed(View::Documents);
                // SQL columns keep the table's order and never include the hidden _id.
                t.fields = match &t.sql {
                    Some(s) => s.cols.iter().map(|c| c.name.clone()).collect(),
                    None => auto_fields(&docs),
                };
                t.paths = doc_paths(&docs);
                let mut f = field_info(&docs);
                if let Some(v) = values.get(&t.coll_key()) {
                    merge_values(&mut f, v);
                }
                t.field_info = Arc::new(f);
                // Checks and open rows follow their documents by _id, so actions that reload
                // (duplicate, filter, edits) keep them; rows that left the results drop out.
                if !t.checked.is_empty() {
                    t.checked = follow_ids(&t.docs, &docs, t.checked.iter().copied());
                    t.check_anchor = None;
                }
                // The open document follows its _id too; rows without one keep their position.
                if let Some((false, i)) = t.selected && t.docs.get(i).is_some_and(|d| d.id().is_some()) {
                    t.selected = follow_ids(&t.docs, &docs, std::iter::once(i)).first().map(|&j| (false, j));
                }
                let open = follow_ids(&t.docs, &docs, t.expanded.iter().filter(|e| !e.0).map(|e| e.1));
                t.expanded.retain(|e| e.0);
                t.expanded.extend(open.into_iter().map(|i| (false, i)));
                t.docs = docs;
                t.count = count;
                t.ms = ms;
                if matches!(t.selected, Some((false, i)) if i >= t.docs.len()) {
                    t.selected = None;
                }
                t.related.clear();
                let ask = (!t.stats_asked && t.sql.is_none()).then(|| t.sel.clone());
                t.stats_asked = true;
                self.load_related(req);
                if let Some(s) = ask && let Some(client) = self.conn(s.conn).and_then(|c| c.client.clone()) {
                    let tag = format!("tabstats\u{1f}{}\u{1f}{}\u{1f}{}", s.conn, s.db, s.coll);
                    self.worker.coll_stats(client, s.db, s.coll, tag);
                }
            }
            Err(e) => {
                // Old rows under the new query would read as its results. Selection and checks are indexes
                // into those rows, so they go too, or the next read would open whatever lands at that index.
                t.docs.clear();
                t.selected = t.selected.filter(|s| s.0);
                t.checked.clear();
                t.expanded.retain(|e| e.0);
                let what = if t.sql.is_some() { "load the rows" } else { "load the documents" };
                self.fail_view(i, View::Documents, e, what);
            }
        }
    }

    fn on_aggregated(&mut self, req: u64, res: db::Res<(Vec<db::Row>, u128)>) {
        let Some(i) = self.tabs.iter().position(|t| t.agg_req == req) else {
            // A superseded run or a closed tab: its error goes to the list, not over the newer run.
            if let Err(e) = res {
                self.err_quiet(e, None, None, "Aggregate".into());
            }
            return;
        };
        let t = &mut self.tabs[i];
        t.agg_req = 0;
        t.rows_replaced();
        match res {
            Ok((d, ms)) => {
                t.clear_failed(View::Aggregate);
                t.agg_fields = auto_fields(&d);
                let n = d.len();
                t.agg_docs = d;
                t.expanded.retain(|e| !e.0);
                t.agg_ms = Some(ms);
                self.ok(format!("Pipeline returned {n} documents"));
            }
            Err(e) => {
                // Old results under a failed run would read as its output.
                (t.agg_docs, t.agg_ms) = (Vec::new(), None);
                self.fail_view(i, View::Aggregate, e, "run the pipeline");
            }
        }
    }

    fn on_written(&mut self, at: (String, String), res: db::Res<String>, undo: Option<UndoAction>, reload: bool, retry: Option<(String, String, String)>) {
        match res {
            Ok(s) => {
                self.reset_doc_state();
                self.ok(s);
            }
            Err(e) => {
                // Reopen the refused insert so the typed document is not lost, but
                // only over the collection it was meant for.
                if let Some((db, coll, text)) = retry && self.t().is_some_and(|t| t.sel.space() == db && t.sel.coll == coll) {
                    self.insert = Some(text);
                }
                self.err(e);
            }
        }
        // An agent's change never joins the user's list; its undo goes back to its Activity entry.
        match undo {
            Some(u) if u.agent.is_some() => {
                let id = u.agent.unwrap_or_default();
                self.agent_keep_undo(id, db::AgentUndo::Sql(u));
            }
            Some(u) => self.push_undo(u),
            None => {}
        }
        // The worker only knows the database and collection, so a same-named
        // collection on another connection may reload too, which is harmless.
        let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.space() == at.0 && self.tabs[i].sel.coll == at.1).collect();
        for i in tabs {
            if reload {
                self.reload_of(i);
            } else {
                self.load_indexes_of(i);
            }
        }
    }

    fn on_saved(&mut self, tag: String, res: db::Res<String>, undo: Option<UndoAction>) {
        let dr = self.saving.remove(&tag);
        let sel = dr.as_ref().map(|d| Sel { conn: d.conn, db: d.db.clone(), schema: d.schema.clone(), coll: d.coll.clone() });
        match res {
            Ok(s) => {
                self.toast_ok(s.clone(), undo.as_ref().map(|u| u.at));
                self.ok(s);
            }
            Err(e) => {
                // Keep the edits so nothing typed is lost; the person can reload and redo. An edit staged on the
                // same row while this save was in flight is newer, so it wins over the failed one.
                if let Some(dr) = dr {
                    self.drafts.entry(tag).or_insert(dr);
                }
                self.err(e);
            }
        }
        if let Some(u) = undo {
            self.push_undo(u);
        }
        // Waits for the collection's other saves so the reload shows all of them.
        if let Some(sel) = sel.filter(|s| !self.saving.values().any(|d| d.conn == s.conn && d.db == s.db && d.schema == s.schema && d.coll == s.coll)) {
            let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel == sel).collect();
            for i in tabs {
                self.reload_of(i);
            }
        }
    }
}

impl App {
    /// Documents per page, from the setting; a hand-edited value is kept in range.
    pub(super) fn page_size(&self) -> u64 {
        self.prefs.page_size.clamp(1, db::PAGE_SIZE_MAX)
    }

    pub(super) fn reload(&mut self) {
        self.reload_of(self.cur);
    }

    pub(super) fn reload_of(&mut self, i: usize) {
        if self.demo {
            return;
        }
        if let Some(t) = self.tabs.get_mut(i) {
            t.agent_changed = false;
        }
        if self.tabs.get(i).is_some_and(|t| t.sql.is_some()) {
            return self.reload_sql(i);
        }
        // A Shell tab opened from the library has no collection to read.
        if self.tabs.get(i).is_some_and(|t| t.sel.coll.is_empty()) {
            return;
        }
        let Some(target) = self.tabs.get(i).and_then(|t| self.target_for(&t.sel)) else { return };
        // Each answer is matched to its tab by id; a slower older one never overwrites a newer one.
        self.req += 1;
        let req = self.req;
        let size = self.page_size();
        let t = &mut self.tabs[i];
        t.req = req;
        t.loading = true;
        t.size = size;
        let (q, page, view, no_idx) = (t.query.clone(), t.page, t.view, t.indexes.is_empty());
        self.worker.find(req, target, q, page, size);
        // Indexes also feed the Sort history (as in the original) and the indexed-field marks, so
        // load them once per tab.
        if view == View::Indexes || no_idx {
            self.load_indexes_of(i);
        }
    }
}
