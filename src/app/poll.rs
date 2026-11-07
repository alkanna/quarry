//! Applies each `db::Msg` from the worker to the app state.

use super::*;

impl App {
    pub(crate) fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Connected { conn, res } => {
                    let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) else { continue };
                    match res {
                        Ok((client, dbs, tunnel)) => {
                            // Trust the SSH host key the first time; later a different key is refused.
                            let mut trusted = None;
                            if let (Some(t), Some(sc)) = (&tunnel, c.meta.ssh.as_mut()) {
                                if sc.known_host.is_none() && !t.fingerprint.is_empty() {
                                    sc.known_host = Some(t.fingerprint.clone());
                                    trusted = Some(t.fingerprint.clone());
                                }
                            }
                            c.tunnel = tunnel;
                            // Load every database's collections up front so the list can show counts.
                            for d in &dbs {
                                self.worker.collections(conn, client.clone(), d.clone());
                            }
                            c.client = Some(client);
                            c.status = Status::Ready;
                            c.dbs = dbs.into_iter().map(|d| (d, None)).collect();
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
                            self.err(e);
                        }
                    }
                }
                Msg::Collections { conn, db, res } => match res {
                    Ok((v, views)) => {
                        self.views.retain(|(c, d, _)| !(*c == conn && *d == db));
                        self.views.extend(views.into_iter().map(|n| (conn, db.clone(), n)));
                        if let Some(d) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == db)) {
                            d.1 = Some(v);
                        }
                    }
                    Err(e) => self.err(format!("Could not list the collections of {db}: {e}")),
                },
                Msg::Command { tag, res } => match (tag.as_str(), res) {
                    ("users", r) => {
                        if let Some(u) = &mut self.users {
                            u.users = Some(r.map(|d| d.get_array("users").map(|a| a.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default()));
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
                    (_, Err(e)) => self.err(e),
                    _ => {}
                },
                Msg::DbChanged { conn, db, res } => {
                    let dropped = self.dropping_db.take_if(|d| d.0 == conn && d.1 == db);
                    match res {
                        Ok(m) => self.ok(m),
                        Err(e) => {
                            self.err(e);
                            if dropped.is_some() {
                                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                                    c.dbs.push((db.clone(), None));
                                    c.dbs.sort_by(|a, b| a.0.cmp(&b.0));
                                }
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
                Msg::IndexInfo { key, res } => match res {
                    Ok(i) => self.idx_info = Some((key, i)),
                    Err(e) => self.err(e),
                },
                Msg::Docs { req, res } => {
                    let Some(t) = self.tab_by_req(req) else { continue };
                    t.loading = false;
                    match res {
                        Ok((docs, count, ms)) => {
                            // A query that works again clears an earlier error.
                            if self.status.1 {
                                self.status = (format!("Loaded {} documents", docs.len()), false);
                            }
                            let t = self.tab_by_req(req).unwrap();
                            t.fields = auto_fields(&docs);
                            t.paths = doc_paths(&docs);
                            t.date_paths = date_paths(&docs, &t.paths);
                            // Checks and open rows follow their documents by _id, so actions that reload
                            // (duplicate, filter, edits) keep them; rows that left the results drop out.
                            if !t.checked.is_empty() {
                                t.checked = follow_ids(&t.docs, &docs, t.checked.iter().copied());
                                t.check_anchor = None;
                            }
                            // The open document follows its _id too; rows without one keep their position.
                            if let Some((false, i)) = t.selected {
                                if t.docs.get(i).is_some_and(|d| d.contains_key("_id")) {
                                    t.selected = follow_ids(&t.docs, &docs, std::iter::once(i)).first().map(|&j| (false, j));
                                }
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
                            self.load_related(req);
                        }
                        Err(e) => {
                            t.docs.clear();
                            self.err(e);
                        }
                    }
                }
                Msg::Indexes { req, res } => match (res, self.tabs.iter_mut().find(|t| t.idx_req == req)) {
                    (Ok(v), Some(t)) => t.indexes = v,
                    (Err(e), _) => self.err(e),
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
                            (st.count, st.sample, st.ms, st.error) = (Some(n), docs, Some(ms), None);
                        }
                        Err(e) => (st.count, st.sample, st.ms, st.error) = (None, Vec::new(), None, Some(e)),
                    }
                }
                Msg::Aggregated { req, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.agg_req == req) else {
                        // A superseded run or a closed tab: its error still deserves a mention.
                        if let Err(e) = res {
                            self.err(e);
                        }
                        continue;
                    };
                    t.agg_req = 0;
                    match res {
                        Ok((d, ms)) => {
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
                            self.err(e);
                        }
                    }
                }
                Msg::Written { at, res, undo, reload, retry } => {
                    match res {
                        Ok(s) => {
                            self.reset_doc_state();
                            self.ok(s);
                        }
                        Err(e) => {
                            // Reopen the refused insert so the typed document is not lost, but
                            // only over the collection it was meant for.
                            if let Some((db, coll, text)) = retry {
                                if self.t().is_some_and(|t| t.sel.db == db && t.sel.coll == coll) {
                                    self.insert = Some(text);
                                }
                            }
                            self.err(e);
                        }
                    }
                    if let Some(u) = undo {
                        self.push_undo(u);
                    }
                    // The worker only knows the database and collection, so a same-named
                    // collection on another connection may reload too, which is harmless.
                    let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.db == at.0 && self.tabs[i].sel.coll == at.1).collect();
                    for i in tabs {
                        if reload {
                            self.reload_of(i);
                        } else {
                            self.load_indexes_of(i);
                        }
                    }
                }
                Msg::Saved { tag, res, undo } => {
                    let dr = self.saving.remove(&tag);
                    let sel = dr.as_ref().map(|d| Sel { conn: d.conn, db: d.db.clone(), coll: d.coll.clone() });
                    match res {
                        Ok(s) => {
                            self.toast = Some((s.clone(), std::time::Instant::now(), undo.as_ref().map(|u| u.at)));
                            self.ok(s);
                        }
                        Err(e) => {
                            // Keep the edits so nothing typed is lost; the person can reload and redo.
                            if let Some(dr) = dr {
                                self.drafts.insert(tag, dr);
                            }
                            self.err(e);
                        }
                    }
                    if let Some(u) = undo {
                        self.push_undo(u);
                    }
                    // Waits for the collection's other saves so the reload shows all of them.
                    if let Some(sel) = sel.filter(|s| !self.saving.values().any(|d| d.conn == s.conn && d.db == s.db && d.coll == s.coll)) {
                        let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel == sel).collect();
                        for i in tabs {
                            self.reload_of(i);
                        }
                    }
                }
                Msg::Info(res) => match res {
                    Ok(s) => self.ok(s),
                    Err(e) => self.err(e),
                },
                Msg::Notice { title, body } => self.modal = Some(Modal::Text { title, body, code: false }),
                Msg::Grouped { conn, db, coll, path, pipeline, res } => match res {
                    Ok(rows) => {
                        self.ok(format!("{} distinct values of {path}", rows.len()));
                        self.modal = Some(Modal::Grouped { sel: Sel { conn, db, coll }, path, pipeline, rows });
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
                    let Some(t) = self.tabs.iter_mut().find(|t| t.compare.req == req) else { continue };
                    t.compare.running = false;
                    match res {
                        Ok(r) => t.compare.result = Some(r),
                        Err(e) => self.err(e),
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
                Msg::Tested { res } => self.conn_test = Some(res),
                Msg::Schema { req, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.schema.req == req) else { continue };
                    t.schema.running = false;
                    match res {
                        Ok((stats, n, ms)) => (t.schema.stats, t.schema.docs, t.schema.ms, t.schema.err) = (stats, n, Some(ms), None),
                        Err(e) => (t.schema.stats, t.schema.ms, t.schema.err) = (Vec::new(), None, Some(e)),
                    }
                }
                Msg::ShellLog { req, level, text } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) {
                        t.shell.logs.push((level, text));
                    }
                }
                Msg::ShellDone { req, res, ms } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) else { continue };
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
                        Ok(v) => sh.set_value(v),
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
}
