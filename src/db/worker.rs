//! The async side: a tokio runtime that runs every Mongo, SSH and tool call and sends one `Msg` back.

use super::*;

pub struct Worker {
    pub(in crate::db) rt: tokio::runtime::Runtime,
    pub(in crate::db) tx: Sender<Msg>,
    pub(in crate::db) ctx: eframe::egui::Context,
}

/// How many leading documents of an ordered insert_many batch were written before `err`.
/// An ordered insert stops at its first write error, so everything before that index is in.
/// A write concern error alone means the whole batch was written but not confirmed.
pub(crate) fn inserted_before(err: &mongodb::error::Error, batch: usize) -> usize {
    match &*err.kind {
        mongodb::error::ErrorKind::InsertMany(f) => f.write_errors.iter().flatten().map(|w| w.index).min().unwrap_or(batch).min(batch),
        _ => 0,
    }
}

pub(crate) fn e(err: impl std::fmt::Display) -> String {
    let s = err.to_string();
    // Driver errors append their labels and the raw reply bytes, which only hide the message.
    let s = match s.strip_prefix("Kind: ") {
        Some(r) => r.split(", labels: ").next().unwrap_or(r).to_string(),
        None => s,
    };
    if (s.contains("imeout") && !s.contains("lock timeout")) || s.contains("exceeded time limit") {
        // Same advice the original gives on timeouts.
        format!("{s}\nThis usually means a query on a non-indexed field of a big collection. Raise the timeout or add an index.")
    } else {
        s
    }
}

impl Worker {
    pub fn new(tx: Sender<Msg>, ctx: eframe::egui::Context) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        Self { rt, tx, ctx }
    }

    /// Runs a Shell script in a separate Bun process; `cancel` stops it.
    pub fn shell(&self, req: u64, job: ShellJob, cancel: tokio::sync::oneshot::Receiver<()>) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let send = move |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let start = Instant::now();
            let res = run_shell(req, &job, &send, cancel).await;
            send(Msg::ShellDone { req, res, ms: start.elapsed().as_millis() });
        });
    }

    /// Streams the collection's changes as `WatchEvent`s until `cancel` fires or its sender drops.
    pub fn watch(&self, req: u64, t: Target, cancel: tokio::sync::oneshot::Receiver<()>) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let send = move |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let run = async {
                // UpdateLookup so an update shows the whole document as it is now, not just the change.
                let mut s = t.c().watch().full_document(mongodb::options::FullDocumentType::UpdateLookup).await.map_err(e)?.with_type::<Document>();
                while let Some(ev) = s.next().await {
                    send(Msg::WatchEvent { req, doc: ev.map_err(e)? });
                }
                Ok(())
            };
            let res = tokio::select! {
                r = run => r,
                _ = cancel => Ok(()),
            };
            send(Msg::WatchDone { req, res });
        });
    }

    /// Starts the shell editor's language server; it stops once the sender drops or after a quiet spell.
    pub fn js_ls(&self, runtime: std::path::PathBuf, dir: std::path::PathBuf, packages: Vec<String>) -> tokio::sync::mpsc::UnboundedSender<JsLsReq> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (out, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let send = move |m: Msg| {
                let _ = out.send(m);
                ctx.request_repaint();
            };
            // Autocomplete falls back to the built-in suggestions, so a failure here is only logged.
            let res = run_js_ls(runtime, dir, packages, rx, &send).await;
            if let Err(err) = &res {
                eprintln!("shell language server: {err}");
            }
            send(Msg::JsLsStopped { failed: res.is_err() });
        });
        tx
    }

    /// Runs blocking work (such as the update check) off the UI thread.
    pub fn blocking(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn_blocking(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    /// Delivers a message through the same channel real results use (test fixtures only).
    pub fn send(&self, m: Msg) {
        self.spawn(async move { m });
    }

    pub(in crate::db) fn spawn(&self, f: impl Future<Output = Msg> + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            // A panic in the task would otherwise end it silently; say so instead.
            let msg = tokio::spawn(f).await.unwrap_or_else(|err| Msg::Info(Err(format!("Internal error: {err}"))));
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// `spawn` for a job that runs SQL, with its statements logged under this connection and database. A job
    /// that skipped the scope would log nothing and nobody would notice, so every SQL job starts here.
    pub(in crate::db) fn spawn_sql(&self, conn: u64, schema: &str, f: impl Future<Output = Msg> + Send + 'static) {
        self.spawn(sql_scope(conn, schema, f));
    }

    /// `attempt` comes back with the answer, so the app can drop one for a connect it no longer waits for.
    pub fn connect(&self, conn: u64, attempt: u64, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move {
            let res = open_client(conn, &uri, timeout, &net).await;
            Msg::Connected { conn, attempt, res }
        });
    }

    /// Connects and lists databases without keeping anything, for the connection form.
    pub fn test(&self, req: u64, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move {
            let res = open_client(0, &uri, timeout, &net).await.map(|(_, dbs, t)| {
                let via = match (&t, &net.proxy) {
                    (Some(t), _) => format!(" through SSH (host key {})", t.fingerprint),
                    (None, Some(p)) => format!(" through the proxy {}:{}", p.host, p.port),
                    _ => String::new(),
                };
                format!("Connected{via}: {} databases.", dbs.len())
            });
            Msg::Tested { req, res, info: None }
        });
    }

    pub fn collections(&self, conn: u64, client: Client, db: String) {
        self.spawn(async move {
            let res = async {
                let specs: Vec<mongodb::results::CollectionSpecification> = client.database(&db).list_collections().await.map_err(e)?.try_collect().await.map_err(e)?;
                let mut names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
                names.sort();
                let views = specs.iter().filter(|s| s.collection_type == mongodb::results::CollectionType::View).map(|s| s.name.clone()).collect();
                Ok((names, views))
            }
            .await;
            Msg::Collections { conn, db, res }
        });
    }

    /// Runs a read command and returns its answer under `tag`.
    pub fn command(&self, client: Client, db: String, cmd: Document, tag: &str) {
        let tag = tag.to_string();
        self.spawn(async move {
            let res = client.database(&db).run_command(cmd).await.map_err(e);
            Msg::Command { tag, res }
        });
    }

    /// Runs a command that changes a database (create, rename, drop, users) and reports `done`.
    pub fn db_change(&self, conn: u64, client: Client, db: String, cmd: Document, done: String) {
        self.spawn(async move {
            // renameCollection only runs on the admin database.
            let on = if cmd.contains_key("renameCollection") { "admin".to_string() } else { db.clone() };
            let res = client.database(&on).run_command(cmd).await.map(|_| done).map_err(e);
            Msg::DbChanged { conn, db, copied: None, res }
        });
    }

    /// Copies a collection's documents, settings and indexes under a new name in the same database.
    pub fn duplicate_coll(&self, conn: u64, client: Client, db: String, from: String, to: String) {
        self.spawn(async move {
            let res = async {
                let d = client.database(&db);
                // Independent reads, so they share one round trip.
                let (info, indexes, taken) = tokio::try_join!(d.run_command(doc! { "listCollections": 1, "filter": { "name": &from } }), d.run_command(doc! { "listIndexes": &from }), d.run_command(doc! { "listCollections": 1, "filter": { "name": &to }, "nameOnly": true })).map_err(e)?;
                let (info, indexes) = (first_batch(info)?, first_batch(indexes)?);
                // Since MongoDB 7 create succeeds on a collection that exists with the same options, so it alone no longer refuses a taken name.
                if !first_batch(taken)?.is_empty() {
                    return Err(format!("{to} already exists in {db}"));
                }
                let mut opts = info.first().and_then(Bson::as_document).and_then(|c| c.get_document("options").ok()).cloned().unwrap_or_default();
                // $merge cannot write into either kind, and a plain copy would quietly drop what makes them special.
                if opts.get_bool("capped").unwrap_or(false) || opts.contains_key("timeseries") {
                    return Err(format!("{from} is capped or time-series and cannot be duplicated here; use Dump and Restore"));
                }
                if let Ok(ci) = opts.get_document_mut("clusteredIndex") {
                    ci.remove("v");
                }
                // Creating the copy first refuses a name taken since the check above with other options, where $out would
                // replace that collection, and keeps the source's validator, collation and other settings.
                let mut create = doc! { "create": &to };
                create.extend(opts);
                d.run_command(create).await.map_err(e)?;
                let src = d.collection::<Document>(&from);
                // Documents that predate the validator must still copy.
                src.aggregate(vec![doc! { "$merge": { "into": &to, "whenMatched": "fail", "whenNotMatched": "insert" } }]).bypass_document_validation(true).await.map_err(e)?;
                // Raw specs keep every index option exactly as the server stores it.
                let specs: Vec<Bson> = indexes
                    .into_iter()
                    .filter_map(|s| match s {
                        Bson::Document(mut s) if s.get_str("name") != Ok("_id_") => {
                            s.remove("ns");
                            Some(Bson::Document(s))
                        }
                        _ => None,
                    })
                    .collect();
                if !specs.is_empty() {
                    d.run_command(doc! { "createIndexes": &to, "indexes": specs }).await.map_err(|err| format!("Copied the documents to {to}, but its indexes failed: {}", e(err)))?;
                }
                Ok(format!("Duplicated {from} to {to}"))
            }
            .await;
            Msg::DbChanged { conn, db, copied: Some(to), res }
        });
    }

    /// Size and count of one collection, from $collStats (the collStats command is deprecated).
    pub fn coll_stats(&self, client: Client, db: String, coll: String, tag: String) {
        self.spawn(async move {
            let res = async {
                let c = client.database(&db).collection::<Document>(&coll);
                let mut cur = c.aggregate(vec![doc! { "$collStats": { "storageStats": {} } }]).await.map_err(e)?;
                let d: Document = cur.try_next().await.map_err(e)?.ok_or("No statistics returned")?;
                let s = d.get_document("storageStats").cloned().unwrap_or_default();
                let mut out = doc! { "collection": format!("{db}.{coll}") };
                for k in ["count", "size", "avgObjSize", "storageSize", "nindexes", "totalIndexSize", "capped"] {
                    if let Some(v) = s.get(k) {
                        out.insert(k, v.clone());
                    }
                }
                if let Ok(sizes) = s.get_document("indexSizes") {
                    out.insert("indexSizes", sizes.clone());
                }
                Ok(out)
            }
            .await;
            Msg::Command { tag, res }
        });
    }

    pub fn find(&self, req: u64, t: Target, q: Query, page: u64, size: u64) {
        self.spawn(async move {
            let res = async {
                let start = Instant::now();
                let c = t.c();
                let (hint, co) = (q.driver_hint(), q.driver_collation()?);
                // An empty filter on a huge collection is the common case; the estimate is instant.
                let count = if q.filter.is_empty() {
                    c.estimated_document_count().max_time(t.timeout).await
                } else {
                    let mut n = c.count_documents(q.filter.clone()).max_time(t.timeout);
                    if let Some(h) = hint.clone() {
                        n = n.hint(h);
                    }
                    if let Some(co) = co.clone() {
                        n = n.collation(co);
                    }
                    n.await
                }
                .map_err(e)?;
                let count = q.limit.map_or(count.saturating_sub(q.skip), |l| count.saturating_sub(q.skip).min(l));
                let take = count.saturating_sub(page * size).min(size).max(1);
                // Read as BSON bytes: the page is kept that way, see `Row`.
                let raw = c.clone_with_type::<mongodb::bson::RawDocumentBuf>();
                let mut f = raw.find(q.filter).skip(q.skip.saturating_add(page * size)).limit(take as i64).max_time(t.timeout);
                if let Some(h) = hint {
                    f = f.hint(h);
                }
                if let Some(co) = co {
                    f = f.collation(co);
                }
                if let Some(s) = q.sort {
                    f = f.sort(s);
                }
                if let Some(p) = q.projection {
                    f = f.projection(p);
                }
                let docs: Vec<Row> = f.await.map_err(e)?.map_ok(Row::from_raw).try_collect().await.map_err(e)?;
                // One row at a time, so checking never holds more than one decoded document.
                for r in &docs {
                    r.decode()?;
                }
                Ok((docs, count, start.elapsed().as_millis()))
            }
            .await;
            Msg::Docs { req, res }
        });
    }

    pub fn indexes(&self, req: u64, t: Target) {
        self.spawn(async move {
            let res = index_specs(&t).await;
            Msg::Indexes { req, res }
        });
    }

    /// Usage, size and hidden state per index, plus the profiler's unindexed or slow queries.
    pub fn index_info(&self, t: Target) {
        self.spawn(async move {
            let key = format!("{}.{}", t.db, t.coll);
            let res = async {
                let db = t.client.database(&t.db);
                let num = |b: Option<&Bson>| match b {
                    Some(Bson::Int32(n)) => *n as i64,
                    Some(Bson::Int64(n)) => *n,
                    Some(Bson::Double(n)) => *n as i64,
                    _ => 0,
                };
                // Sizes are best effort: views and some deployments refuse $collStats.
                let sizes = async {
                    let mut cur = t.c().aggregate(vec![doc! { "$collStats": { "storageStats": {} } }]).await.ok()?;
                    let d = cur.try_next().await.ok()??;
                    d.get_document("storageStats").ok()?.get_document("indexSizes").ok().cloned()
                };
                // Best effort too: usage counts are extra, and a refusal must not hide the suggestions.
                let rows = async { t.c().aggregate(vec![doc! { "$indexStats": {} }]).await.map_err(e)?.try_collect::<Vec<Document>>().await.map_err(e) };
                // Independent reads, asked together: one round trip instead of three.
                let (sizes, rows, prof) = tokio::join!(sizes, rows, db.run_command(doc! { "profile": -1 }));
                let (sizes, prof) = (sizes.unwrap_or_default(), prof.ok());
                let (rows, stats_err) = match rows {
                    Ok(r) => (r, None),
                    Err(err) => (Vec::new(), Some(err)),
                };
                let stats = rows
                    .iter()
                    .map(|d| {
                        let name = d.get_str("name").unwrap_or_default().to_string();
                        let acc = d.get_document("accesses").cloned().unwrap_or_default();
                        IndexStat {
                            ops: num(acc.get("ops")),
                            since: acc.get_datetime("since").ok().map(|t| t.timestamp_millis()),
                            hidden: d.get_document("spec").is_ok_and(|s| s.get_bool("hidden") == Ok(true)),
                            size: sizes.get(&name).map(|b| num(Some(b)) as usize),
                            name,
                        }
                    })
                    .collect();
                let level = prof.as_ref().map_or(0, |p| num(p.get("was")) as i32);
                let slowms = prof.as_ref().map_or(100, |p| num(p.get("slowms")));
                // Newest first so the cap keeps recent traffic.
                let entries: Vec<Document> = match db
                    .collection::<Document>("system.profile")
                    .aggregate(vec![
                        doc! { "$match": { "ns": &key, "op": "query", "command.find": { "$exists": true }, "$or": [{ "planSummary": { "$regex": "COLLSCAN" } }, { "millis": { "$gt": slowms } }] } },
                        doc! { "$sort": { "ts": -1 } },
                        doc! { "$limit": 1000 },
                        doc! { "$project": { "filter": "$command.filter", "sort": "$command.sort", "millis": 1 } },
                    ])
                    .await
                {
                    Ok(c) => c.try_collect().await.unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                Ok(IndexInfo { stats, stats_err, level, slowms, shapes: group_profile(&entries) })
            }
            .await;
            Msg::IndexInfo { key, res }
        });
    }

    /// Preview of a pipeline cut after one stage, as the original's getResultsForStage (renderer
    /// line 112026): a `$count` run for the total and a `$limit` run for the first documents. An
    /// empty pipeline is the collection itself, counted by the estimate (getCollectionSample, 111469).
    pub fn stage_preview(&self, req: u64, t: Target, pipeline: Vec<Document>, limit: i64) {
        self.spawn(async move {
            let res = async {
                let start = Instant::now();
                let c = t.c();
                // `{ $match: {} }` keeps every document, so it is dropped: a pipeline of only those is the
                // collection itself, counted from its metadata instead of a scan that times out.
                let mut pipeline = pipeline;
                pipeline.retain(|s| !(s.len() == 1 && s.get_document("$match").is_ok_and(|m| m.is_empty())));
                let (n, docs) = if pipeline.is_empty() {
                    let n = c.estimated_document_count().max_time(t.timeout).await.map_err(e)?;
                    let docs: Vec<Document> = c.find(doc! {}).limit(limit).max_time(t.timeout).await.map_err(e)?.try_collect().await.map_err(e)?;
                    (Some(n), docs)
                } else {
                    let mut sampling = pipeline.clone();
                    sampling.push(doc! { "$limit": limit });
                    let mut counting = pipeline;
                    counting.push(doc! { "$count": "total" });
                    // Side by side, so a slow count does not hold the sample back.
                    let (docs, counted) = tokio::join!(
                        async { c.aggregate(sampling).max_time(t.timeout).allow_disk_use(true).await?.try_collect::<Vec<Document>>().await },
                        async { c.aggregate(counting).max_time(t.timeout).allow_disk_use(true).await?.try_collect::<Vec<Document>>().await },
                    );
                    let docs = docs.map_err(e)?;
                    // The count reads every document out of the stage, so it can run out of time where
                    // the sample does not. It is left out then; any other failure is a real error.
                    let n = match counted {
                        Ok(d) => Some(d.first().and_then(|d| d.get("total")).map_or(Ok(0), as_u64)?),
                        Err(err) if matches!(&*err.kind, mongodb::error::ErrorKind::Command(c) if c.code == 50) => None,
                        Err(err) => return Err(e(err)),
                    };
                    (n, docs)
                };
                Ok((n, trim_sample(docs), start.elapsed().as_millis()))
            }
            .await;
            Msg::StagePreview { req, res }
        });
    }

    pub fn aggregate(&self, req: u64, t: Target, pipeline: Vec<Document>) {
        self.spawn(async move {
            let res = async {
                let start = Instant::now();
                // Kept as BSON bytes like a Documents page, see `Row`.
                let cur = t.c().aggregate(pipeline).with_type::<mongodb::bson::RawDocumentBuf>().max_time(t.timeout).allow_disk_use(true).await.map_err(e)?;
                // ponytail: results capped at 1000 in memory; stream to a file via export for more.
                let docs: Vec<Row> = cur.take(1000).map_ok(Row::from_raw).try_collect().await.map_err(e)?;
                for r in &docs {
                    r.decode()?;
                }
                Ok((docs, start.elapsed().as_millis()))
            }
            .await;
            Msg::Aggregated { req, res }
        });
    }

    pub fn explain(&self, req: u64, t: Target, what: ExplainOf, size: u64) {
        self.spawn(async move {
            let inner = match what {
                ExplainOf::Find(q) => {
                    let mut d = doc! { "find": &t.coll, "filter": q.filter, "limit": q.limit.map_or(size, |l| l.min(size)) as i64 };
                    if q.skip > 0 {
                        d.insert("skip", q.skip as i64);
                    }
                    if let Some(s) = q.sort {
                        d.insert("sort", s);
                    }
                    if let Some(p) = q.projection {
                        d.insert("projection", p);
                    }
                    if let Some(h) = q.hint {
                        d.insert("hint", h);
                    }
                    if let Some(co) = q.collation {
                        d.insert("collation", co);
                    }
                    d
                }
                ExplainOf::Aggregate(p) => doc! { "aggregate": &t.coll, "pipeline": p, "cursor": {} },
            };
            let cmd = doc! { "explain": inner, "verbosity": "executionStats", "maxTimeMS": t.timeout.as_millis() as i64 };
            let res = t.client.database(&t.db).run_command(cmd).await.map_err(e);
            Msg::Explained { req, res }
        });
    }

    fn write(&self, at: (String, String), f: impl Future<Output = (Res<String>, Option<UndoAction>)> + Send + 'static) {
        self.spawn(async move {
            let (res, undo) = f.await;
            Msg::Written { at, res, undo, reload: true, retry: None }
        });
    }

    pub fn insert(&self, conn: u64, t: Target, d: Document, text: Option<String>) {
        self.spawn(async move {
            let at = t.at();
            let retry = text.map(|x| (t.db.clone(), t.coll.clone(), x));
            let (res, undo) = match t.c().insert_one(d).await {
                Ok(r) => (
                    Ok(format!("Inserted document {}", short(&r.inserted_id))),
                    Some(UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::Insert { id: r.inserted_id } }),
                ),
                Err(err) => (Err(e(err)), None),
            };
            Msg::Written { at, res, undo, reload: true, retry }
        });
    }

    /// Replaces `old` with `new`, but only if the server still holds exactly `old`.
    pub fn replace(&self, conn: u64, t: Target, old: Document, new: Document, record_undo: bool) {
        self.write(t.at(), replace_doc(conn, t, old, new, record_undo));
    }

    /// Writes one document's staged edits; the answer names the draft so it can be kept on failure.
    pub fn save(&self, tag: String, conn: u64, t: Target, old: Document, new: Document) {
        self.spawn(async move {
            let (res, undo) = replace_doc(conn, t, old, new, true).await;
            Msg::Saved { tag, res, undo }
        });
    }

    pub fn delete(&self, conn: u64, t: Target, old: Document, record_undo: bool) {
        self.write(t.at(), async move {
            match t.c().delete_one(unchanged(&old)).await {
                Ok(r) if r.deleted_count == 0 => (Err(CONFLICT.into()), None),
                Ok(_) => (
                    Ok("Deleted 1 document".into()),
                    record_undo.then(|| UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::Delete { old } }),
                ),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    /// Deletes each document only while the server still holds it as loaded, like `delete`.
    /// Undo restores exactly the ones that went, never ones changed or removed meanwhile.
    pub fn delete_docs(&self, conn: u64, t: Target, old: Vec<Document>) {
        self.write(t.at(), async move {
            let c = t.c();
            let total = old.len();
            let mut gone = Vec::new();
            for d in old {
                match c.delete_one(unchanged(&d)).await {
                    Ok(r) if r.deleted_count == 1 => gone.push(d),
                    Ok(_) => {}
                    Err(err) => {
                        let undo = (!gone.is_empty()).then(|| UndoAction { at: Instant::now(), conn, db: t.db.clone(), coll: t.coll.clone(), kind: UndoKind::DeleteMany { old: gone.clone() } });
                        return (Err(format!("Stopped after deleting {}: {}", gone.len(), e(err))), undo);
                    }
                }
            }
            let skipped = total - gone.len();
            if gone.is_empty() {
                return (Err(CONFLICT.into()), None);
            }
            let mut msg = format!("Deleted {} documents", gone.len());
            if skipped > 0 {
                msg += &format!(", {skipped} skipped because they changed or were already deleted");
            }
            (Ok(msg), Some(UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::DeleteMany { old: gone } }))
        });
    }

    pub fn delete_by_id(&self, t: Target, id: Bson) {
        self.write(t.at(), async move {
            match t.c().delete_one(doc! { "_id": id }).await {
                Ok(r) => (Ok(format!("Undo removed {} document", r.deleted_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    /// Preview values for the referenced ids shown in the table (relatedDocuments, renderer line 107207).
    pub fn related(&self, req: u64, t: Target, ids: Vec<Bson>, preview: String) {
        self.spawn(async move {
            let res = async {
                let c = t.c();
                let f = c.find(doc! { "_id": { "$in": ids } }).projection(doc! { &preview: 1 }).max_time(t.timeout);
                let docs: Vec<Document> = f.await.map_err(e)?.try_collect().await.map_err(e)?;
                Ok(docs.iter().filter_map(|d| Some((compact(d.get("_id")?), value_text(get_path(d, &preview)?)))).collect())
            }
            .await;
            Msg::Related { req, res }
        });
    }

    /// Tries each collection of the database for a document with this `_id` (findReference,
    /// renderer line 105588). `t.coll` is the collection the id was found in.
    pub fn find_ref(&self, conn: u64, t: Target, colls: Vec<String>, path: String, value: Bson) {
        self.spawn(async move {
            let res = async {
                let base = t.client.database(&t.db);
                for c in colls.iter().filter(|c| !c.starts_with("system.")) {
                    if let Some(d) = base.collection::<Document>(c).find_one(doc! { "_id": value.clone() }).max_time(t.timeout).await.map_err(e)? {
                        return Ok(Some((c.clone(), preview_field(&d))));
                    }
                }
                Ok(None)
            }
            .await;
            Msg::RefFound { conn, db: t.db, coll: t.coll, path, value, res }
        });
    }

    /// Finds relations of `colls` by sampling their 10 newest documents and looking their id-like
    /// values up in every collection of the database (analyzeDataRelations, renderer line 105620).
    pub fn analyze_relations(&self, conn: u64, t: Target, colls: Vec<String>, all: Vec<String>) {
        self.spawn(async move {
            let res = async {
                let base = t.client.database(&t.db);
                let mut found = Vec::new();
                for coll in &colls {
                    let docs: Vec<Document> = base.collection::<Document>(coll).find(doc! {}).sort(doc! { "$natural": -1 }).limit(10).max_time(t.timeout).await.map_err(e)?.try_collect().await.map_err(e)?;
                    // String ids count only when this collection itself uses string ids of that length.
                    let id_len = match docs.first().and_then(|d| d.get("_id")) {
                        Some(Bson::String(s)) => s.len(),
                        _ => 0,
                    };
                    for (path, ids) in proto_relations(&docs, id_len) {
                        for target in &all {
                            let hit = base.collection::<Document>(target).find_one(doc! { "_id": { "$in": ids.clone() } }).max_time(t.timeout).await;
                            if let Ok(Some(d)) = hit {
                                found.push((coll.clone(), path.clone(), target.clone(), preview_field(&d)));
                                break;
                            }
                        }
                    }
                }
                Ok(found)
            }
            .await;
            Msg::Analyzed { conn, db: t.db, res }
        });
    }

    /// Loads both collections whole and compares them by `_id`.
    // ponytail: full documents are fetched; hash them server-side if collections near COMPARE_MAX get slow
    pub fn compare(&self, req: u64, src: Target, dst: Target) {
        self.spawn(async move {
            let res = async {
                let mut sides = Vec::new();
                for t in [&src, &dst] {
                    let n = t.c().estimated_document_count().await.map_err(e)?;
                    if n > COMPARE_MAX {
                        return Err(format!("{}.{} has {n} documents; comparing is limited to {COMPARE_MAX}.", t.db, t.coll));
                    }
                    let docs: Vec<Document> = t.c().find(doc! {}).await.map_err(e)?.try_collect().await.map_err(e)?;
                    sides.push(docs);
                }
                let dst_docs = sides.pop().unwrap_or_default();
                Ok(compare_docs(sides.pop().unwrap_or_default(), dst_docs))
            }
            .await;
            Msg::Compared { req, res }
        });
    }

    /// Writes chosen differences into the target. A changed document is only replaced while it is
    /// still as compared, so edits made meanwhile are kept.
    pub fn sync(&self, t: Target, insert: Vec<Document>, replace: Vec<(Document, Document)>, delete: Vec<Bson>) {
        self.write(t.at(), async move {
            let r = async {
                let c = t.c();
                for ch in insert.chunks(SYNC_CHUNK) {
                    c.insert_many(ch.to_vec()).await.map_err(e)?;
                }
                let mut skipped = 0;
                for (new, old) in &replace {
                    if c.replace_one(unchanged(old), new.clone()).await.map_err(e)?.matched_count == 0 {
                        skipped += 1;
                    }
                }
                let mut deleted = 0;
                for ch in delete.chunks(SYNC_CHUNK) {
                    deleted += c.delete_many(doc! { "_id": { "$in": ch.to_vec() } }).await.map_err(e)?.deleted_count;
                }
                let mut msg = format!("Sync done: {} inserted, {} replaced, {deleted} deleted", insert.len(), replace.len() - skipped);
                if skipped > 0 {
                    msg += &format!(", {skipped} skipped because they changed since the comparison");
                }
                Ok(msg)
            }
            .await;
            (r, None)
        });
    }

    /// Runs mongodump or mongorestore, streaming its output; `cancel` stops it.
    pub fn tool(&self, exe: std::path::PathBuf, args: Vec<String>, cancel: tokio::sync::oneshot::Receiver<()>) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let send = move |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let name = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            // The address carries the password, and arguments are visible to every local user.
            let mut args = args;
            let mut config = None;
            if let Some(i) = args.iter().position(|a| a.starts_with("--uri=")) {
                match write_tool_config(&args[i]["--uri=".len()..]) {
                    Ok(p) => {
                        args[i] = format!("--config={}", p.display());
                        config = Some(p);
                    }
                    Err(err) => return send(Msg::ToolDone(Err(format!("Could not write the {name} config ({err})")))),
                }
            }
            let run = async {
                use tokio::io::AsyncBufReadExt;
                let mut cmd = tokio::process::Command::new(&exe);
                cmd.args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
                #[cfg(windows)]
                cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
                let mut child = cmd
                    .spawn()
                    .map_err(|err| format!("Could not start {} ({err})", exe.display()))?;
                // Both tools write their progress to stderr.
                let mut err = tokio::io::BufReader::new(child.stderr.take().ok_or("no stderr")?).lines();
                let mut out = tokio::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
                let (mut err_open, mut out_open) = (true, true);
                while err_open || out_open {
                    tokio::select! {
                        l = err.next_line(), if err_open => match l.map_err(e)? { Some(l) => send(Msg::ToolLog(l)), None => err_open = false },
                        l = out.next_line(), if out_open => match l.map_err(e)? { Some(l) => send(Msg::ToolLog(l)), None => out_open = false },
                    }
                }
                let st = child.wait().await.map_err(e)?;
                if st.success() { Ok(format!("{name} finished")) } else { Err(format!("{name} failed ({st})")) }
            };
            let res = tokio::select! {
                r = run => r,
                _ = cancel => Err(format!("{name} stopped")),
            };
            if let Some(p) = config {
                let _ = std::fs::remove_file(p);
            }
            send(Msg::ToolDone(res));
        });
    }

    /// Runs "Group by and count" in the background like the original's groupCountField
    /// (renderer line 100487), which shows the result in a window instead of a tab.
    // ponytail: first 1000 values kept; a field with millions of distinct values would not fit a window anyway
    pub fn group_count(&self, conn: u64, t: Target, path: String, pipeline: Vec<Document>, co: Option<Document>) {
        self.spawn(async move {
            let p = pipeline.clone();
            let res = async {
                let c = t.c();
                let mut a = c.aggregate(p).max_time(t.timeout).allow_disk_use(true);
                if let Some(co) = collation_of(co) {
                    a = a.collation(co);
                }
                a.await.map_err(e)?.take(1000).try_collect().await.map_err(e)
            }
            .await;
            Msg::Grouped { conn, db: t.db, coll: t.coll, path, pipeline, res }
        });
    }

    /// A column data action over the documents matching `filter`, at most COL_VALUES_CAP of them in
    /// query order. Group results come back as `Msg::Grouped`, like the field menu's group by.
    pub fn column_values(&self, conn: u64, t: Target, op: ColOp, path: String, scope: Scope, sort: Option<Document>) {
        let Scope { filter, collation: co } = scope;
        self.spawn(async move {
            let pipeline = group_count_pipeline(&path, &Bson::Null, filter.clone()).unwrap_or_default();
            let res = async {
                // Only the top-level field is projected: a numeric segment may be an array position, which
                // find cannot project, or a plain key, which stripping would lose. get_path picks the value.
                let top = path.split('.').next().unwrap_or_default();
                let c = t.c();
                let mut f = c.find(filter).projection(doc! { top: 1 }).limit(COL_VALUES_CAP).max_time(t.timeout);
                if let Some(co) = collation_of(co) {
                    f = f.collation(co);
                }
                if let Some(s) = sort {
                    f = f.sort(s);
                }
                // Streamed so each document is dropped once its value is taken.
                let mut cursor = f.await.map_err(e)?;
                let (mut values, mut n) = (Vec::new(), 0);
                while let Some(d) = cursor.try_next().await.map_err(e)? {
                    n += 1;
                    values.extend(get_path(&d, &path).cloned());
                }
                Ok((col_op_result(op, &values), n >= COL_VALUES_CAP))
            }
            .await;
            match res {
                Ok((ColOut::Groups(rows), _)) => Msg::Grouped { conn, db: t.db, coll: t.coll, path, pipeline, res: Ok(rows) },
                res => Msg::ColumnValues { op, coll: t.coll, path, res },
            }
        });
    }

    /// A field's most frequent values across the collection, for the query box's suggestions, from a
    /// random sample: `distinct` is complete but unbounded, and on unindexed fields of big collections
    /// it times out, while the sample's cost stays the same at any size. With `prefix`, the first
    /// 1000 documents whose value starts with it instead (an anchored regex can use an index), or
    /// None when that would scan a big collection.
    pub fn field_values(&self, key: String, t: Target, path: String, prefix: Option<String>) {
        self.spawn(async move {
            let res = async {
                // A prefix nobody has makes an unindexed field scan the whole collection, so typed
                // lookups run only where they are cheap: the field leads an index, or the collection
                // is small. Both checks read metadata only. Views answer neither and are skipped.
                if prefix.is_some() {
                    let c = t.c();
                    let (count, specs) = tokio::join!(async { c.estimated_document_count().max_time(t.timeout).await }, index_specs(&t));
                    let small = count.is_ok_and(|n| n <= PREFIX_SCAN_MAX);
                    let leads = specs.is_ok_and(|s| s.iter().any(|ix| ix.get_document("keys").ok().and_then(|k| k.keys().next()) == Some(&path)));
                    if !small && !leads {
                        return Ok(None);
                    }
                }
                // Only kinds the list offers, so ids or objects cannot take every slot.
                let mut keep = doc! { "$type": ["string", "int", "long", "double", "bool"] };
                let mut pipeline = vec![doc! { "$sample": { "size": 1000 } }];
                if let Some(p) = &prefix {
                    let re = format!("^{}", regex_escape(p));
                    pipeline = vec![doc! { "$match": { &path: { "$regex": &re } } }, doc! { "$limit": 1000 }];
                    keep.insert("$regex", re);
                }
                pipeline.extend([
                    doc! { "$project": { "v": format!("${path}") } },
                    doc! { "$unwind": "$v" },
                    doc! { "$match": { "v": keep } },
                    doc! { "$group": { "_id": "$v", "n": { "$sum": 1 } } },
                    doc! { "$sort": { "n": -1, "_id": 1 } },
                    doc! { "$limit": FIELD_VALUES_MAX as i64 },
                ]);
                // Named in the query log, and cut short: suggestions are not worth a long wait.
                let mut cursor = t.c().aggregate(pipeline).comment("quarry: value suggestions").max_time(t.timeout.min(Duration::from_secs(2))).await.map_err(e)?;
                let mut out = Vec::new();
                while let Some(d) = cursor.try_next().await.map_err(e)? {
                    out.extend(d.get("_id").cloned());
                }
                Ok(Some(out))
            }
            .await;
            Msg::FieldValues { key, path, prefix, res }
        });
    }

    /// Like the original's "Count distinct values": distinct(path, current filter).len().
    pub fn distinct_count(&self, t: Target, path: String, scope: Scope) {
        let Scope { filter, collation: co } = scope;
        self.spawn(async move {
            let c = t.c();
            let mut d = c.distinct(&path, filter).max_time(t.timeout);
            if let Some(co) = collation_of(co) {
                d = d.collation(co);
            }
            match d.await {
                Ok(v) => Msg::Notice { title: "Number of distinct values".into(), body: format!("{}\n\ndistinct values in \"{path}\" (the current query is applied)", v.len()) },
                Err(err) => Msg::Info(Err(e(err))),
            }
        });
    }

    /// Runs the real update stages on a sample, so the preview cannot differ from the write.
    pub fn update_preview(&self, t: Target, scope: Scope, stages: Vec<Document>) {
        let Scope { filter, collation: co } = scope;
        self.spawn(async move {
            let res = async {
                let co = collation_of(co);
                let c = t.c();
                let mut a = c.aggregate(vec![doc! { "$match": &filter }, doc! { "$limit": 5 }]).max_time(t.timeout);
                if let Some(co) = co.clone() {
                    a = a.collation(co);
                }
                let before: Vec<Document> = a.await.map_err(e)?.try_collect().await.map_err(e)?;
                // The same documents by _id, so each pair is one document even if others change meanwhile.
                let ids: Vec<Bson> = before.iter().filter_map(|d| d.get("_id").cloned()).collect();
                let after_pipe = std::iter::once(doc! { "$match": { "_id": { "$in": ids } } }).chain(stages.iter().cloned()).collect::<Vec<_>>();
                let mut after: std::collections::HashMap<String, Document> = t.c().aggregate(after_pipe).max_time(t.timeout).await.map_err(e)?.try_collect::<Vec<_>>().await.map_err(e)?.into_iter().map(|d| (d.get("_id").map(compact).unwrap_or_default(), d)).collect();
                let pairs = before.into_iter().filter_map(|b| after.remove(&b.get("_id").map(compact).unwrap_or_default()).map(|a| (b, a))).collect();
                let c = t.c();
                let mut n = c.count_documents(filter).max_time(t.timeout);
                if let Some(co) = co {
                    n = n.collation(co);
                }
                let n = n.await.map_err(e)?;
                Ok((pairs, n))
            }
            .await;
            Msg::UpdatePreview { stages, res }
        });
    }

    pub fn update_many(&self, t: Target, scope: Scope, stages: Vec<Document>) {
        let Scope { filter, collation: co } = scope;
        self.write(t.at(), async move {
            let c = t.c();
            let mut u = c.update_many(filter, stages);
            if let Some(co) = collation_of(co) {
                u = u.collation(co);
            }
            match u.await {
                Ok(r) => (Ok(format!("Updated {} documents", r.modified_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    pub fn rename_many(&self, t: Target, scope: Scope, from: String, to: String) {
        let Scope { filter, collation: co } = scope;
        self.write(t.at(), async move {
            let c = t.c();
            let mut u = c.update_many(rename_filter(filter, &to), doc! { "$rename": { &from: &to } });
            if let Some(co) = collation_of(co) {
                u = u.collation(co);
            }
            match u.await {
                Ok(r) => (Ok(format!("Renamed {from} to {to} in {} documents; any that already had {to} were left as they were", r.modified_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    /// Writes imported documents: insert all (ordered, in batches), or upsert each by `key` with
    /// $set like the original's import (bulkWrite of updateOne upserts).
    pub fn import(&self, conn: u64, t: Target, docs: Vec<Document>, upsert_by: Option<String>) {
        const BATCH: usize = 1000;
        self.write(t.at(), async move {
            let c = t.c();
            let mut docs = docs;
            match upsert_by {
                None => {
                    // Like the original's generateNewDocumentId: documents without _id get one of the
                    // kind the collection already uses. ObjectIds are made here rather than by the
                    // driver so a failed batch still knows the ids of what it did insert.
                    if docs.iter().any(|d| !d.contains_key("_id")) {
                        let last = c.find_one(doc! {}).sort(doc! { "_id": -1 }).await.ok().flatten().and_then(|d| d.get("_id").cloned());
                        assign_ids(&mut docs, last);
                    }
                    let mut ids = Vec::with_capacity(docs.len());
                    for chunk in docs.chunks(BATCH) {
                        match c.insert_many(chunk.to_vec()).await {
                            Ok(r) => ids.extend(r.inserted_ids.into_values()),
                            Err(err) => {
                                let done = inserted_before(&err, chunk.len());
                                ids.extend(chunk[..done].iter().filter_map(|d| d.get("_id").cloned()));
                                let msg = format!("Stopped after {} documents: {}", ids.len(), e(err));
                                let undo = (!ids.is_empty()).then(|| UndoAction { at: Instant::now(), conn, db: t.db.clone(), coll: t.coll.clone(), kind: UndoKind::InsertMany { ids } });
                                return (Err(msg), undo);
                            }
                        }
                    }
                    let n = ids.len();
                    (Ok(format!("Imported {n} documents")), Some(UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::InsertMany { ids } }))
                }
                Some(key) => {
                    let (mut matched, mut upserted) = (0u64, 0u64);
                    for d in docs {
                        let Some(v) = d.get(&key).cloned() else {
                            return (Err(format!("A document has no {key}; nothing after it was written ({matched} updated, {upserted} added).")), None);
                        };
                        match c.update_one(doc! { &key: v }, doc! { "$set": d }).upsert(true).await {
                            Ok(r) => {
                                matched += r.matched_count;
                                upserted += r.upserted_id.is_some() as u64;
                            }
                            Err(err) => return (Err(format!("Stopped ({matched} updated, {upserted} added): {}", e(err))), None),
                        }
                    }
                    (Ok(format!("Import done: {matched} updated, {upserted} added")), None)
                }
            }
        });
    }

    pub fn delete_ids(&self, t: Target, ids: Vec<Bson>) {
        self.write(t.at(), async move {
            match t.c().delete_many(doc! { "_id": { "$in": ids } }).await {
                Ok(r) => (Ok(format!("Undo removed {} imported documents", r.deleted_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    /// Reads up to `limit` documents (0 = all) matching `filter` and summarises their fields.
    /// `last` reads the most recently inserted ones, in reverse natural order: `_id` is not always
    /// an ObjectId, so sorting on it would not mean recent.
    pub fn schema(&self, req: u64, t: Target, scope: Scope, limit: i64, last: bool, maps: Vec<String>) {
        let Scope { filter, collation: co } = scope;
        self.spawn(async move {
            let res = async {
                let start = Instant::now();
                let c = t.c();
                let mut f = c.find(filter).max_time(t.timeout);
                if let Some(co) = collation_of(co) {
                    f = f.collation(co);
                }
                if limit > 0 {
                    f = f.limit(limit);
                }
                if last {
                    f = f.sort(doc! { "$natural": -1 });
                }
                let docs: Vec<Document> = f.await.map_err(e)?.try_collect().await.map_err(e)?;
                let stats = analyze_schema(&docs, &maps);
                Ok((stats, docs.len(), start.elapsed().as_millis()))
            }
            .await;
            Msg::Schema { req, res }
        });
    }

    pub fn delete_many(&self, t: Target, scope: Scope) {
        let Scope { filter, collation: co } = scope;
        self.write(t.at(), async move {
            let c = t.c();
            let mut d = c.delete_many(filter);
            if let Some(co) = collation_of(co) {
                d = d.collation(co);
            }
            match d.await {
                Ok(r) => (Ok(format!("Deleted {} documents", r.deleted_count)), None),
                Err(err) => (Err(e(err)), None),
            }
        });
    }

    pub fn drop_index(&self, t: Target, name: String) {
        self.spawn(async move {
            let res = t.c().drop_index(&name).await.map(|_| format!("Dropped index {name}")).map_err(e);
            Msg::Written { at: t.at(), res, undo: None, reload: false, retry: None }
        });
    }

    pub fn export(&self, t: Target, q: Query, path: std::path::PathBuf) {
        self.spawn(async move {
            let res = async {
                let c = t.c();
                let (hint, co) = (q.driver_hint(), q.driver_collation()?);
                let mut f = c.find(q.filter).skip(q.skip).max_time(t.timeout);
                if let Some(h) = hint {
                    f = f.hint(h);
                }
                if let Some(co) = co {
                    f = f.collation(co);
                }
                if let Some(l) = q.limit {
                    f = f.limit(l as i64);
                }
                if let Some(s) = q.sort {
                    f = f.sort(s);
                }
                if let Some(p) = q.projection {
                    f = f.projection(p);
                }
                // ponytail: whole result held in memory (CSV needs every column first); stream if exports get huge.
                let docs: Vec<Document> = f.await.map_err(e)?.try_collect().await.map_err(e)?;
                let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("json").to_ascii_lowercase();
                let body = export_body(&docs, &ext)?;
                std::fs::write(&path, body).map_err(|x| format!("Could not write {}: {x}", path.display()))?;
                Ok(format!("Exported {} documents to {}", docs.len(), path.display()))
            }
            .await;
            Msg::Info(res)
        });
    }
}

pub(crate) async fn open_client(conn: u64, uri: &str, timeout: Duration, net: &Net) -> Res<(Client, Vec<String>, Option<crate::ssh::Tunnel>)> {
    use mongodb::options::Socks5Proxy;
    let tunnel = match &net.ssh {
        Some((conf, secrets)) => Some(crate::ssh::open(conf, secrets, timeout, None).await?),
        None => None,
    };
    let mut o = ClientOptions::parse(uri).await.map_err(e)?;
    o.connect_timeout = Some(timeout);
    o.server_selection_timeout = Some(timeout);
    o.app_name.get_or_insert_with(|| "quarry".into());
    o.command_event_handler = Some(mongodb::event::EventHandler::callback(move |ev| log_event(conn, ev)));
    if let Some(t) = &tunnel {
        o.socks5_proxy = Some(Socks5Proxy::builder().host("127.0.0.1".to_string()).port(t.port).build());
    } else if let Some(p) = &net.proxy {
        // The original's proxyHost/proxyPort/proxyUsername/proxyPassword; port 1080 when unset.
        let auth = (!p.user.is_empty()).then(|| (p.user.clone(), p.password.clone().unwrap_or_default()));
        o.socks5_proxy = Some(Socks5Proxy::builder().host(p.host.clone()).port(if p.port == 0 { 1080 } else { p.port }).authentication(auth).build());
    }
    let client = Client::with_options(o).map_err(e)?;
    let mut dbs = client.list_database_names().await.map_err(e)?;
    dbs.sort();
    Ok((client, dbs, tunnel))
}

pub(crate) async fn replace_doc(conn: u64, t: Target, old: Document, new: Document, record_undo: bool) -> (Res<String>, Option<UndoAction>) {
    let id = old.get("_id").cloned().unwrap_or(Bson::Null);
    match t.c().replace_one(unchanged(&old), new.clone()).await {
        Ok(r) if r.matched_count == 0 => (Err(CONFLICT.into()), None),
        Ok(_) => (
            Ok(format!("Saved document {}", short(&id))),
            record_undo.then(|| UndoAction { at: Instant::now(), conn, db: t.db, coll: t.coll, kind: UndoKind::Update { old, new } }),
        ),
        Err(err) => (Err(e(err)), None),
    }
}

/// An index spec as the app keeps it: name and keys first, then every option the index was made
/// with, without the format version and the old namespace field.
/// The collection's index specs, raw rather than the driver's IndexModel, which drops options it
/// does not model. A collection has at most 64 indexes, so the first batch holds them all.
async fn index_specs(t: &Target) -> Res<Vec<Document>> {
    let specs = first_batch(t.client.database(&t.db).run_command(doc! { "listIndexes": &t.coll }).await.map_err(e)?)?;
    Ok(specs.iter().filter_map(Bson::as_document).map(index_doc).collect())
}

/// The results of a command that answers with a cursor, like listIndexes or listCollections.
fn first_batch(mut reply: Document) -> Res<Vec<Bson>> {
    reply.get_document_mut("cursor").and_then(|c| c.get_array_mut("firstBatch")).map(std::mem::take).map_err(e)
}

pub(crate) fn index_doc(spec: &Document) -> Document {
    let mut d = doc! { "name": spec.get_str("name").unwrap_or_default(), "keys": spec.get("key").cloned().unwrap_or_else(|| Bson::Document(doc! {})) };
    for (k, v) in spec.iter().filter(|(k, _)| !["name", "key", "v", "ns"].contains(&k.as_str())) {
        d.insert(k, v.clone());
    }
    d
}

/// Gives each document without an _id one of the kind the collection's `last` _id has: the next
/// number, a random string of the same length, or an ObjectId.
pub(crate) fn assign_ids(docs: &mut [Document], last: Option<Bson>) {
    let mut next = match &last {
        Some(Bson::Int32(n)) => Some(*n as i64 + 1),
        Some(Bson::Int64(n)) => Some(n + 1),
        Some(Bson::Double(f)) => Some(*f as i64 + 1),
        _ => None,
    };
    for d in docs.iter_mut().filter(|d| !d.contains_key("_id")) {
        let id = match (&last, next.as_mut()) {
            (Some(Bson::String(s)), _) => Bson::String(random_id(s.chars().count())),
            (_, Some(n)) => {
                *n += 1;
                Bson::Int64(*n - 1)
            }
            _ => Bson::ObjectId(mongodb::bson::oid::ObjectId::new()),
        };
        let mut with_id = doc! { "_id": id };
        with_id.extend(std::mem::take(d));
        *d = with_id;
    }
}

/// $rename replaces a field that already has the new name, so documents that have one are left out.
pub(crate) fn rename_filter(filter: Document, to: &str) -> Document {
    doc! { "$and": [filter, { to: { "$exists": false } }] }
}

pub const CONFLICT: &str = "Not saved: this document changed on the server since you opened it. Reload and try again.";

/// Matches the document only while every field is still exactly as we last read it,
/// so a concurrent change by someone else is never silently overwritten.
pub(crate) fn unchanged(old: &Document) -> Document {
    doc! {
        "_id": old.get("_id").cloned().unwrap_or(Bson::Null),
        "$expr": { "$eq": ["$$ROOT", { "$literal": old.clone() }] },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_insert_count() {
        use mongodb::error::{Error, ErrorKind, InsertManyError};
        let failed = |d: Document| Error::from(ErrorKind::InsertMany(mongodb::bson::from_document::<InsertManyError>(d).unwrap()));
        assert_eq!(inserted_before(&failed(doc! { "writeErrors": [{ "index": 3, "code": 11000, "errmsg": "dup" }] }), 10), 3);
        assert_eq!(inserted_before(&failed(doc! { "writeConcernError": { "code": 64, "errmsg": "wc" } }), 10), 10);
        assert_eq!(inserted_before(&Error::from(std::io::ErrorKind::ConnectionReset), 10), 0);
    }

    #[test]
    fn driver_errors_drop_labels_and_raw_reply() {
        let raw = "Kind: Command failed: Error code 59 (CommandNotFound): no such command: 'nope', labels: {}, source: None, server response: Some(RawDocumentBuf { data: \"5d00\" })";
        assert_eq!(e(raw), "Command failed: Error code 59 (CommandNotFound): no such command: 'nope'");
        assert_eq!(e("plain"), "plain");
    }

    #[test]
    fn connects_through_socks5_by_name() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = tokio::spawn(async move {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut head = [0u8; 2];
                s.read_exact(&mut head).await.unwrap();
                let mut m = vec![0u8; head[1] as usize];
                s.read_exact(&mut m).await.unwrap();
                s.write_all(&[5, 0]).await.unwrap();
                let mut req = [0u8; 5];
                s.read_exact(&mut req).await.unwrap();
                assert_eq!(req[3], 3, "the host should be sent as a name");
                let mut name = vec![0u8; req[4] as usize];
                s.read_exact(&mut name).await.unwrap();
                let mut p = [0u8; 2];
                s.read_exact(&mut p).await.unwrap();
                let _ = s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await;
                (String::from_utf8(name).unwrap(), u16::from_be_bytes(p))
            });
            let net = Net { proxy: Some(Proxy { host: "127.0.0.1".into(), port, user: String::new(), password: None }), ..Default::default() };
            let r = open_client(0, "mongodb://db.internal.example:27018/", Duration::from_millis(1500), &net).await;
            assert!(r.is_err());
            assert_eq!(seen.await.unwrap(), ("db.internal.example".to_string(), 27018));
        });
    }

    #[test]
    fn writes_match_the_whole_document_as_read() {
        let old = doc! { "_id": 7, "a": { "b": 1 } };
        assert_eq!(unchanged(&old), doc! { "_id": 7, "$expr": { "$eq": ["$$ROOT", { "$literal": old.clone() }] } });
        assert_eq!(rename_filter(doc! { "x": 1 }, "y"), doc! { "$and": [{ "x": 1 }, { "y": { "$exists": false } }] });
    }

    #[test]
    fn imported_ids_follow_the_collection() {
        let mut docs = vec![doc! { "a": 1 }, doc! { "_id": 99, "a": 2 }, doc! { "a": 3 }];
        assign_ids(&mut docs, Some(Bson::Int32(10)));
        assert_eq!(docs.iter().map(|d| d.get("_id").cloned().unwrap()).collect::<Vec<_>>(), [Bson::Int64(11), Bson::Int32(99), Bson::Int64(12)]);
        // _id comes first, the way the original writes them.
        assert_eq!(docs[0].keys().next().unwrap(), "_id");
        let mut docs = vec![doc! {}];
        assign_ids(&mut docs, Some(Bson::String("abcd".into())));
        assert_eq!(docs[0].get_str("_id").unwrap().chars().count(), 4);
        let mut docs = vec![doc! {}];
        assign_ids(&mut docs, None);
        assert!(docs[0].get_object_id("_id").is_ok());
    }

    /// A worker and an empty collection on the server at QUARRY_TEST_MONGO, or None to skip.
    /// ponytail: one collection per test name, dropped at the start, so a failed run leaves no trap.
    fn live(name: &str) -> Option<(Worker, std::sync::mpsc::Receiver<Msg>, Target)> {
        live_at("QUARRY_TEST_MONGO", name)
    }

    fn live_at(var: &str, name: &str) -> Option<(Worker, std::sync::mpsc::Receiver<Msg>, Target)> {
        let uri = std::env::var(var).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let t = w.rt.block_on(async {
            let client = Client::with_uri_str(&uri).await.unwrap();
            let t = Target { client, db: "quarry_test".into(), coll: name.into(), timeout: Duration::from_secs(5) };
            t.c().drop().await.unwrap();
            t
        });
        Some((w, rx, t))
    }

    fn next(rx: &std::sync::mpsc::Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(10)).expect("worker answer")
    }

    fn all(w: &Worker, t: &Target) -> Vec<Document> {
        w.rt.block_on(async { t.c().find(doc! {}).sort(doc! { "_id": 1 }).await.unwrap().try_collect().await.unwrap() })
    }

    fn seed(w: &Worker, t: &Target, docs: Vec<Document>) {
        w.rt.block_on(async { t.c().insert_many(docs).await.unwrap() });
    }

    #[test]
    fn live_find_pages_rows_as_bson_bytes() {
        let Some((w, rx, t)) = live("find_rows") else { return };
        seed(&w, &t, (0..120).map(|i| doc! { "_id": i, "n": i % 7, "nested": { "list": [i, { "deep": format!("v{i}") }] } }).collect());
        let q = Query { filter: doc! { "n": { "$lt": 5 } }, sort: Some(doc! { "_id": -1 }), projection: Some(doc! { "nested": 1 }), ..Default::default() };
        w.find(1, t.clone(), q, 1, PAGE_SIZE);
        let Msg::Docs { res: Ok((rows, count, _)), .. } = next(&rx) else { panic!("expected Docs") };
        let want: Vec<Document> = (0..120).rev().filter(|i| i % 7 < 5).skip(PAGE_SIZE as usize).take(PAGE_SIZE as usize).map(|i| doc! { "_id": i, "nested": { "list": [i, { "deep": format!("v{i}") }] } }).collect();
        assert_eq!(count, (0..120).filter(|i| i % 7 < 5).count() as u64);
        assert_eq!(rows.iter().map(|r| r.decode().unwrap()).collect::<Vec<_>>(), want);
        let first = &rows[0];
        assert_eq!(first.id(), want[0].get("_id").cloned());
        assert_eq!(first.value("nested.list.1.deep").and_then(|v| Bson::try_from(v).ok()), crate::db::get_path(&want[0], "nested.list.1.deep").cloned());
    }

    #[test]
    fn live_duplicate_copies_documents_and_indexes_and_refuses_a_taken_name() {
        let Some((w, rx, t)) = live("dup_src") else { return };
        let copy = Target { coll: "dup_copy".into(), ..t.clone() };
        w.rt.block_on(async { copy.c().drop().await.unwrap() });
        seed(&w, &t, (0..10).map(|i| doc! { "_id": i, "k": i }).collect());
        w.rt.block_on(async {
            t.c().create_index(mongodb::IndexModel::builder().keys(doc! { "k": 1 }).build()).await.unwrap();
            t.client.database(&t.db).run_command(doc! { "collMod": &t.coll, "validator": { "k": { "$gte": 0 } } }).await.unwrap();
        });
        w.duplicate_coll(0, t.client.clone(), t.db.clone(), t.coll.clone(), copy.coll.clone());
        let Msg::DbChanged { res, .. } = next(&rx) else { panic!("expected DbChanged") };
        res.unwrap();
        assert_eq!(all(&w, &copy), all(&w, &t));
        let names: Vec<String> = w.rt.block_on(async { copy.c().list_index_names().await.unwrap() });
        assert!(names.contains(&"k_1".to_string()));
        let specs: Vec<mongodb::results::CollectionSpecification> = w.rt.block_on(async { t.client.database(&t.db).list_collections().filter(doc! { "name": &copy.coll }).await.unwrap().try_collect().await.unwrap() });
        assert!(specs[0].options.validator.is_some());
        // A second copy onto the same name is refused instead of replacing it.
        w.duplicate_coll(0, t.client.clone(), t.db.clone(), t.coll.clone(), copy.coll.clone());
        let Msg::DbChanged { res, .. } = next(&rx) else { panic!("expected DbChanged") };
        let err = res.unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn live_aggregate_returns_rows_as_bson_bytes() {
        let Some((w, rx, t)) = live("agg_rows") else { return };
        seed(&w, &t, (0..1500).map(|i| doc! { "_id": i, "g": i % 3 }).collect());
        w.aggregate(1, t.clone(), vec![doc! { "$group": { "_id": "$g", "n": { "$sum": 1 } } }, doc! { "$sort": { "_id": 1 } }]);
        let Msg::Aggregated { res: Ok((rows, _)), .. } = next(&rx) else { panic!("expected Aggregated") };
        assert_eq!(rows.iter().map(|r| r.decode().unwrap()).collect::<Vec<_>>(), [doc! { "_id": 0, "n": 500 }, doc! { "_id": 1, "n": 500 }, doc! { "_id": 2, "n": 500 }]);
        w.aggregate(2, t.clone(), vec![doc! { "$match": {} }]);
        let Msg::Aggregated { res: Ok((rows, _)), .. } = next(&rx) else { panic!("expected Aggregated") };
        assert_eq!(rows.len(), 1000);
    }

    #[test]
    fn live_save_refuses_a_document_changed_meanwhile() {
        let Some((w, rx, t)) = live("save_conflict") else { return };
        seed(&w, &t, vec![doc! { "_id": 1, "a": 1 }]);
        let stale = doc! { "_id": 1, "a": 0 };
        w.save("k".into(), 1, t.clone(), stale, doc! { "_id": 1, "a": 2 });
        let Msg::Saved { res, undo, .. } = next(&rx) else { panic!("expected Saved") };
        assert_eq!(res, Err(CONFLICT.to_string()));
        assert!(undo.is_none());
        assert_eq!(all(&w, &t), [doc! { "_id": 1, "a": 1 }]);
        w.save("k".into(), 1, t.clone(), doc! { "_id": 1, "a": 1 }, doc! { "_id": 1, "a": 2 });
        let Msg::Saved { res, .. } = next(&rx) else { panic!("expected Saved") };
        assert!(res.is_ok());
        assert_eq!(all(&w, &t), [doc! { "_id": 1, "a": 2 }]);
    }

    #[test]
    fn live_delete_refuses_a_document_changed_meanwhile() {
        let Some((w, rx, t)) = live("delete_conflict") else { return };
        seed(&w, &t, vec![doc! { "_id": 1, "a": 1 }]);
        w.delete(1, t.clone(), doc! { "_id": 1, "a": 0 }, true);
        let Msg::Written { res, .. } = next(&rx) else { panic!("expected Written") };
        assert_eq!(res, Err(CONFLICT.to_string()));
        assert_eq!(all(&w, &t).len(), 1);
    }

    #[test]
    fn live_failed_import_undoes_exactly_what_it_wrote() {
        let Some((w, rx, t)) = live("import_rollback") else { return };
        seed(&w, &t, vec![doc! { "_id": 3, "pre": true }]);
        let docs = (1..=5).map(|i| doc! { "_id": i, "n": i }).collect();
        w.import(1, t.clone(), docs, None);
        let Msg::Written { res, undo, .. } = next(&rx) else { panic!("expected Written") };
        assert!(res.unwrap_err().starts_with("Stopped after 2 documents"));
        let Some(UndoAction { kind: UndoKind::InsertMany { ids }, .. }) = undo else { panic!("expected an undo") };
        assert_eq!(ids, [Bson::Int32(1), Bson::Int32(2)]);
        w.delete_ids(t.clone(), ids);
        next(&rx);
        assert_eq!(all(&w, &t), [doc! { "_id": 3, "pre": true }]);
    }

    #[test]
    fn live_index_list_keeps_every_option() {
        let Some((w, rx, t)) = live("index_options") else { return };
        let ttl = doc! { "key": { "at": 1 }, "name": "ttl", "expireAfterSeconds": 60, "sparse": true, "hidden": true, "collation": { "locale": "fr", "strength": 2 } };
        let partial = doc! { "key": { "x": 1 }, "name": "partial", "partialFilterExpression": { "x": { "$gt": 1 } } };
        w.rt.block_on(async { t.client.database(&t.db).run_command(doc! { "createIndexes": &t.coll, "indexes": [ttl, partial] }).await.unwrap() });
        w.indexes(1, t.clone());
        let Msg::Indexes { res, .. } = next(&rx) else { panic!("expected Indexes") };
        let list = res.unwrap();
        let ix = |name: &str| list.iter().find(|d| d.get_str("name") == Ok(name)).unwrap().clone();
        let ttl = ix("ttl");
        assert_eq!(ttl.get_document("keys").unwrap(), &doc! { "at": 1 });
        assert_eq!(ttl.get_i64("expireAfterSeconds").or(ttl.get_i32("expireAfterSeconds").map(i64::from)), Ok(60));
        for k in ["sparse", "hidden"] {
            assert_eq!(ttl.get_bool(k), Ok(true), "{k}");
        }
        assert_eq!(ttl.get_document("collation").unwrap().get_str("locale"), Ok("fr"));
        assert!(ttl.get("v").is_none());
        assert!(ix("partial").get_document("partialFilterExpression").is_ok());
    }

    /// Needs a replica set: QUARRY_TEST_MONGO_RS, like mongodb://localhost:27018/?directConnection=true.
    #[test]
    fn live_watch_streams_changes_until_stopped() {
        let Some((w, rx, t)) = live_at("QUARRY_TEST_MONGO_RS", "watch") else { return };
        let (stop, cancel) = tokio::sync::oneshot::channel();
        w.watch(1, t.clone(), cancel);
        // The stream starts asynchronously; keep writing until its first event arrives.
        let ev = (0..50).find_map(|i| {
            w.rt.block_on(async { t.c().insert_one(doc! { "_id": i, "n": i }).await.unwrap() });
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Msg::WatchEvent { doc, .. }) => Some(doc),
                _ => None,
            }
        });
        let ev = ev.expect("a change event");
        assert_eq!(ev.get_str("operationType"), Ok("insert"));
        assert!(ev.get_document("fullDocument").is_ok());
        drop(stop);
        let done = std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(5)).ok()).find(|m| matches!(m, Msg::WatchDone { .. }));
        assert!(matches!(done, Some(Msg::WatchDone { req: 1, res: Ok(()) })));
    }

    #[test]
    fn live_bulk_rename_skips_documents_that_have_the_new_name() {
        let Some((w, rx, t)) = live("bulk_rename") else { return };
        seed(&w, &t, vec![doc! { "_id": 1, "a": 1 }, doc! { "_id": 2, "a": 2, "b": "keep" }]);
        w.rename_many(t.clone(), Scope::default(), "a".into(), "b".into());
        let Msg::Written { res, .. } = next(&rx) else { panic!("expected Written") };
        assert_eq!(res.unwrap(), "Renamed a to b in 1 documents; any that already had b were left as they were");
        assert_eq!(all(&w, &t), [doc! { "_id": 1, "b": 1 }, doc! { "_id": 2, "a": 2, "b": "keep" }]);
    }
}
